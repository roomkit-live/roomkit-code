//! Private, room-scoped author attachments. No workspace paths enter the protocol.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use roomkit::model::{CompositeContent, EventContent, MediaContent, RoomId};
use roomkit::transcript::Image;
use roomkit_ai::ImageResolver;
use roomkit_tui::protocol::{ImageUpload, MAX_IMAGE_BYTES, MAX_IMAGE_PIXELS, MAX_IMAGES};
use sha2::{Digest, Sha256};
use std::{
    future::Future,
    io::{Cursor, Read, Write},
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

pub(crate) struct Images {
    root: PathBuf,
}
impl Images {
    pub fn new(root: PathBuf) -> Arc<Self> {
        Arc::new(Self { root })
    }
    fn directory(&self, room: &RoomId) -> PathBuf {
        self.root
            .join(format!("{:x}", Sha256::digest(room.as_str().as_bytes())))
    }
    pub async fn prepare(
        self: &Arc<Self>,
        room: &RoomId,
        text: &str,
        images: &[ImageUpload],
    ) -> Result<EventContent, String> {
        if images.is_empty() || images.len() > MAX_IMAGES || text.len() > 65_536 {
            return Err("Attach between one and four images with at most 64 KiB of text".into());
        }
        let store = self.clone();
        let room = room.clone();
        let text = text.to_owned();
        let images = images.to_vec();
        tokio::task::spawn_blocking(move || store.save(&room, &text, &images))
            .await
            .map_err(|_| "Attachment worker stopped")?
    }
    fn save(
        &self,
        room: &RoomId,
        text: &str,
        images: &[ImageUpload],
    ) -> Result<EventContent, String> {
        // Validate the complete batch before writing any attachment.
        let decoded = images.iter().map(validate).collect::<Result<Vec<_>, _>>()?;
        private_directory(&self.root).map_err(|_| "Cannot open private attachment storage")?;
        let directory = self.directory(room);
        private_directory(&directory)
            .map_err(|_| "Cannot open this session's attachment storage")?;
        let mut parts = Vec::new();
        if !text.is_empty() {
            parts.push(EventContent::text(text));
        }
        for (index, (image, bytes)) in images.iter().zip(decoded).enumerate() {
            let digest = format!("{:x}", Sha256::digest(&bytes));
            let path = directory.join(format!("{digest}.png"));
            match std::fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    let mut file = tempfile::NamedTempFile::new_in(&directory)
                        .map_err(|_| "Cannot create private image file")?;
                    file.write_all(&bytes)
                        .map_err(|_| "Cannot save attachment")?;
                    file.as_file()
                        .sync_all()
                        .map_err(|_| "Cannot flush attachment")?;
                    file.persist_noclobber(&path)
                        .map_err(|_| "Cannot finish saving attachment")?;
                }
                _ => return Err("Attachment storage contains an invalid file".into()),
            }
            let mut media = MediaContent::new(format!("attachment://{digest}"), "image/png");
            media.filename = Some(image.name.clone());
            media.caption = Some(format!(
                "Image {} · {}×{}",
                index + 1,
                image.width,
                image.height
            ));
            media.size_bytes = Some(bytes.len() as u64);
            parts.push(EventContent::Media(media));
        }
        CompositeContent::new(parts)
            .map(EventContent::Composite)
            .map_err(|_| "Cannot compose image message".into())
    }
    fn load(&self, image: &Image, room: &RoomId) -> Option<Image> {
        let digest = image.url().strip_prefix("attachment://")?;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return None;
        }
        let directory = self.directory(room);
        for path in [&self.root, &directory] {
            let metadata = std::fs::symlink_metadata(path).ok()?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return None;
            }
        }
        let path = directory.join(format!("{digest}.png"));
        let metadata = std::fs::symlink_metadata(&path).ok()?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.len() > MAX_IMAGE_BYTES as u64
        {
            return None;
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .ok()?
            .take((MAX_IMAGE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() > MAX_IMAGE_BYTES || format!("{:x}", Sha256::digest(&bytes)) != digest {
            return None;
        }
        Image::new(
            &format!("data:image/png;base64,{}", STANDARD.encode(bytes)),
            "image/png",
        )
    }
}
impl ImageResolver for Images {
    fn resolve<'a>(
        &'a self,
        image: &'a Image,
        room: &'a RoomId,
    ) -> Pin<Box<dyn Future<Output = Option<Image>> + Send + 'a>> {
        let store = Self {
            root: self.root.clone(),
        };
        let image = image.clone();
        let room = room.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || store.load(&image, &room))
                .await
                .ok()
                .flatten()
        })
    }
}
fn private_directory(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = std::fs::symlink_metadata(path)?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(std::io::Error::other("Invalid attachment directory"));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o077 != 0 {
                    return Err(std::io::Error::other("Attachment directory is not private"));
                }
            }
            Ok(())
        }
        Err(error) => Err(error),
    }
}
fn validate(image: &ImageUpload) -> Result<Vec<u8>, String> {
    if image.mime_type != "image/png"
        || image.data.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4
        || image.name.is_empty()
        || image.name.len() > 128
        || image.name.contains(['/', '\\'])
        || image.name.chars().any(char::is_control)
        || image.width == 0
        || image.height == 0
        || u64::from(image.width) * u64::from(image.height) > MAX_IMAGE_PIXELS
    {
        return Err("Invalid attachment: use a PNG up to 4 MiB and 16 million pixels".into());
    }
    let bytes = STANDARD
        .decode(&image.data)
        .map_err(|_| "Invalid image encoding")?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err("Image exceeds 4 MiB".into());
    }
    let mut decoder = png::Decoder::new(Cursor::new(&bytes));
    decoder.set_limits(png::Limits {
        bytes: 128 * 1024 * 1024,
    });
    let mut reader = decoder.read_info().map_err(|_| "Invalid PNG image")?;
    if reader.info().width != image.width
        || reader.info().height != image.height
        || reader.info().animation_control.is_some()
    {
        return Err("Image dimensions differ or the PNG is animated".into());
    }
    let size = reader
        .output_buffer_size()
        .filter(|size| *size <= 128 * 1024 * 1024)
        .ok_or("Decoded image is too large")?;
    reader
        .next_frame(&mut vec![0; size])
        .map_err(|_| "Incomplete or damaged PNG image")?;
    Ok(bytes)
}

#[cfg(test)]
pub(crate) mod tests;
