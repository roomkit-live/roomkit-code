//! What every agent of a launch shares: the workspace's tools, processes,
//! plans and the services their turns report to.
use super::*;

/// Application-scoped resources survive turn, agent and connection changes.
/// Tool adapters derive archive/process ownership from the room and agent IDs.
pub(crate) struct Resources {
    pub messages: Arc<crate::messages::Messages>,
    pub instructions: Arc<crate::instructions::Instructions>,
    /// Each conversation's compactions, for the agents whose memory compacts.
    pub compactions: Arc<crate::compaction::Compactions>,
    pub plans: Arc<crate::plans::Plans>,
    /// Files each conversation's agents changed or read.
    pub files: Arc<crate::files::Files>,
    /// The application's own guide, shared by every agent.
    pub docs: Arc<dyn DocsBackend>,
    pub mutations: Workspace,
    pub processes: ProcessSession,
    /// Every agent's shell: one, which follows the background jobs its
    /// commands leave until they end, and stops them when the application
    /// closes.
    pub shell: ShellTool,
    pub(super) command_env: roomkit_tools_process::CommandEnv,
    pub outputs: OutputStore,
    pub search: WebSearchTool,
    pub fetch: WebFetchTool,
    /// The agents a task can be handed to, which the profiles keep current.
    pub roster: Arc<crate::delegation::Roster>,
    /// What the status line says of each conversation's latest task.
    pub tasks: Arc<crate::delegation::Tasks>,
    /// What each turn is told of how its agent's previous turn ended, and
    /// of a restart since.
    pub turn_ends: Arc<crate::turn_ends::TurnEnds>,
}

impl Resources {
    pub async fn new(
        root: &Path,
        state: &Path,
        global: &Path,
        inherit_env: &[String],
        unattended: bool,
    ) -> Result<Self> {
        let command_env = crate::command_env::command_env(inherit_env);
        let mutations = Workspace::new(root).await?;
        // Small immediate pages leave room for the JSON envelope in the bounded
        // terminal projection. Large captures remain in separately paged archives.
        let output_limits = OutputLimits {
            max_read_bytes: 4096,
            ..OutputLimits::default()
        };
        let outputs = OutputStore::new(Some(state), output_limits).await?;
        let processes = ProcessSession::new(root)
            .await?
            .with_limits(process_limits())?
            .with_env(command_env.clone())?
            .with_output_store(outputs.clone())?;
        // Its background commands are processes: listed, read and told.
        let shell = ShellTool::new(root)
            .await?
            .with_processes(processes.clone())
            .with_limits(shell_limits(unattended))?
            .with_env(command_env.clone())?
            .with_output_store(outputs.clone());
        let search_limits = SearchLimits {
            max_output_bytes: 48 * 1024,
            ..SearchLimits::default()
        };
        let fetch_limits = WebFetchLimits {
            max_output_bytes: 8192,
            ..WebFetchLimits::default()
        };
        let plans = Arc::new(crate::plans::Plans::new(state.join("plans")));
        let files = crate::files::Files::new(root);
        let compactions = Arc::new(crate::compaction::Compactions::new(
            &state.join("compactions"),
        ));
        // A summary carries the plans and the files changed, as recorded.
        compactions.carry(crate::compaction::Recorded::new(
            plans.clone(),
            files.clone(),
        ));
        Ok(Self {
            instructions: crate::instructions::Instructions::new(root, global),
            compactions,
            plans,
            files,
            docs: crate::docs::guide(global)?,
            mutations,
            processes,
            shell,
            command_env,
            outputs,
            search: WebSearchTool::new(Arc::new(ExaSearch::new(SearchHttpConfig::default())?))
                .with_limits(search_limits)?,
            fetch: WebFetchTool::new().with_limits(fetch_limits)?,
            roster: Arc::default(),
            tasks: Arc::default(),
            turn_ends: Arc::default(),
            messages: Arc::default(),
        })
    }

    /// Release every resource. `leave_processes` lets the running processes
    /// go instead of stopping them: an unattended launch exits while a server
    /// it started must still answer the check that follows.
    pub async fn close(&self, leave_processes: bool) -> bool {
        let plans = self.plans.close().await;
        let (processes, shell) = if leave_processes {
            (self.processes.detach().await, self.shell.detach().await)
        } else {
            (self.processes.close().await, self.shell.close().await)
        };
        let outputs = self.outputs.close().await;
        plans && processes && shell && outputs
    }

    /// Tell each agent when a background job it started ends, a process or
    /// what a shell command left running, from now until the resources
    /// close: the agent reads it in a turn of its own, or in the turn it is
    /// running.
    pub fn follow_job_ends(&self, kit: &roomkit::RoomKit<roomkit_sqlite::SqliteStore>) {
        for ends in [self.processes.ends(), self.shell.ends()] {
            let kit = kit.clone();
            tokio::spawn(async move { roomkit_tools_roomkit::notify_job_ends(&kit, ends).await });
        }
    }
}

/// The limits of every agent's processes: 4 KiB read pages, as for the
/// output archives, and reads that wait as long as a terminal shell command
/// may run, within a tool call's timeout. The system prompt states their
/// maximum `timeout_ms`.
pub(crate) fn process_limits() -> ProcessLimits {
    ProcessLimits {
        max_read_bytes: 4096,
        max_wait_ms: crate::timeouts::shell_max_seconds(false) * 1000,
        ..ProcessLimits::default()
    }
}
