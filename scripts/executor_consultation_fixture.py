"""A live question, one reply, and an attempted write in the requesting Plan-mode task."""
import json
import re
import sqlite3
import threading

from executor_fixture import tool

AUTHOR = "investigate-reproduction"
REVIEW = "consult-about-failure"
ENDING = "finish-investigation"
UNANSWERED = "ask-finishing-peer"
QUESTION = "Which command reproduces the failure?"
LAST_QUESTION = "Which input triggered the failure?"


class Consultation:
    def __init__(self):
        self.working = threading.Event()
        self.release = threading.Event()
        self.answer = None
        self.denial = None

    def reply(self, task, messages):
        results = {m["tool_call_id"]: str(m["content"]) for m in messages if m["role"] == "tool"}
        if task in (AUTHOR, ENDING):
            if "reproduction" not in results:
                self.working.set()
                assert self.release.wait(40), "No question reached the parent"
                return tool("read", "reproduction", path="reproduction.txt")
            if "reply" in results:
                assert "answered" in results["reply"], results["reply"]
                return {"content": "Reproduction command supplied."}
            message = next((m["content"] for m in messages if m["role"] == "assistant"
                            and str(m.get("content")).startswith("Peer consultation.")), None)
            if message is None:
                # Steering reaches the agent at a step boundary after the parent records the question.
                return tool("read", f"continue-{len(results)}", path="reproduction.txt")
            payload = json.loads(message.split("\n", 1)[1])
            assert payload["question"] == (LAST_QUESTION if task == ENDING else QUESTION)
            if task == ENDING:
                return {"content": "Investigation finished without replying."}
            command = re.search(r"cargo test repro_[a-f0-9]+", results["reproduction"]).group()
            return tool("answer_peer", "reply", question_id=payload["question_id"], answer=command)

        if "question" not in results:
            context = next(m["content"] for m in messages if "peer_task_id" in str(m.get("content", "")))
            peer = re.search(r'task-[a-f0-9]+(?=[\s"\']*[,}])', context).group()
            return tool("ask_peer", "question", task_id=peer,
                        question=LAST_QUESTION if task == UNANSWERED else QUESTION)
        if task == UNANSWERED:
            assert "peer turn ended before answering" in results["question"].lower(), results["question"]
            return {"content": "Peer ended while I was waiting."}
        self.answer = results["question"]
        if "plan-write" not in results:
            return tool("write", "plan-write", path="peer-output.txt", content="should be refused")
        self.denial = results["plan-write"]
        return {"content": "Consultation finished."}


def exercise(term, server, expected):
    term.send(AUTHOR, "Submitted " + AUTHOR + ".")
    assert server.peer.working.wait(20), "The answering task has no running turn"
    offset = term.send(REVIEW, "Submitted " + REVIEW + ".")
    try:
        term.wait(QUESTION, offset)
    finally:
        server.peer.release.set()
    term.wait("Peer answer", offset)
    term.wait(expected, offset)
    term.wait("Consultation returned to the parent.", offset)
    assert expected in server.peer.answer and "plan" in server.peer.denial.lower()
    print("PASS  Sibling tasks consulted in Plan mode, the requester received the command, "
          "the parent saw the exchange, and the write was refused.", flush=True)


def peer_ends(term, server):
    server.peer.working.clear()
    server.peer.release.clear()
    term.send(ENDING, "Submitted " + ENDING + ".")
    assert server.peer.working.wait(20)
    offset = term.send(UNANSWERED, "Submitted " + UNANSWERED + ".")
    try:
        term.wait(LAST_QUESTION, offset)
    finally:
        server.peer.release.set()
    term.wait("Peer closed", offset)
    term.wait("Unanswered consultation returned to the parent.", offset)
    print("PASS  Ending the peer turn released the asker before the timeout.", flush=True)


def check_history(database, expected):
    with sqlite3.connect(database) as db:
        rows = db.execute("SELECT room_id, data FROM events WHERE id LIKE 'peer-%' ORDER BY idx").fetchall()
    assert len(rows) == 4 and len({room for room, _ in rows}) == 1, rows
    texts = [json.loads(data)["content"]["body"] for _, data in rows]
    assert texts[0].startswith("Peer question · macbook (") and QUESTION in texts[0], texts
    assert texts[1].startswith("Peer answer · linux (") and expected in texts[1], texts
    assert texts[2].startswith("Peer question · macbook (") and LAST_QUESTION in texts[2], texts
    assert texts[3].startswith("Peer closed"), texts
