use super::*;
#[test]
fn private_attachments_are_validated_and_scoped_to_their_room() -> crate::Result<()> {
    let state = tempfile::tempdir()?;
    let root = state.path().join("attachments");
    let store = Images::new(root.clone());
    let room = RoomId::new("session-one")?;
    let image = fixture()?;
    let content = store.save(&room, "What is this?", std::slice::from_ref(&image))?;
    let EventContent::Composite(composite) = content else {
        return Err("Expected composite".into());
    };
    let Some(EventContent::Media(media)) = composite.parts().last() else {
        return Err("Expected image".into());
    };
    let image_ref = Image::new(&media.url, &media.mime_type).ok_or("Missing image")?;
    assert_eq!(
        store.load(&image_ref, &room).map(|i| i.url().to_owned()),
        Some(format!("data:image/png;base64,{}", image.data))
    );
    assert!(
        store
            .load(&image_ref, &RoomId::new("session-two")?)
            .is_none()
    );
    assert!(!serde_json::to_string(&composite)?.contains(&image.data));
    let path = store.directory(&room).join(format!(
        "{}.png",
        media.url.trim_start_matches("attachment://")
    ));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path)?.permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::metadata(root)?.permissions().mode() & 0o777, 0o700);
    }
    // Reopened stores resolve the same room; damaged files never reach the provider.
    let reopened = Images::new(state.path().join("attachments"));
    assert!(reopened.load(&image_ref, &room).is_some());
    std::fs::write(path, b"damaged")?;
    assert!(reopened.load(&image_ref, &room).is_none());
    Ok(())
}
#[test]
fn invalid_batch_writes_nothing() -> crate::Result<()> {
    let state = tempfile::tempdir()?;
    let root = state.path().join("attachments");
    let store = Images::new(root.clone());
    let image = fixture()?;
    let mut bad = image.clone();
    bad.width = 2;
    assert!(
        store
            .save(&RoomId::new("fixture")?, "", &[image, bad])
            .is_err()
    );
    assert!(!root.exists());
    let mut bad = fixture()?;
    bad.data = STANDARD.encode(b"not an image");
    assert!(validate(&bad).is_err());
    bad = fixture()?;
    bad.name = "../secret".into();
    assert!(validate(&bad).is_err());
    Ok(())
}
pub(crate) fn fixture() -> crate::Result<ImageUpload> {
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&[255, 0, 0, 255])?;
        writer.finish()?;
    }
    Ok(ImageUpload {
        name: "fixture.png".into(),
        mime_type: "image/png".into(),
        data: STANDARD.encode(bytes),
        width: 1,
        height: 1,
    })
}
