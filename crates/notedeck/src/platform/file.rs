use std::{path::PathBuf, str::FromStr};

use crossbeam_channel::{unbounded, Receiver, Sender};
use once_cell::sync::Lazy;

use crate::{Error, SupportedMimeType};

#[derive(Debug)]
pub enum MediaFrom {
    PathBuf(PathBuf),
    Memory(Vec<u8>),
}

#[derive(Debug)]
pub struct SelectedMedia {
    pub from: MediaFrom,
    pub file_name: String,
    pub media_type: SupportedMimeType,
}

impl SelectedMedia {
    pub fn from_path(path: PathBuf) -> Result<Self, Error> {
        if let Some(ex) = path.extension().and_then(|f| f.to_str()) {
            let media_type = SupportedMimeType::from_extension(ex)?;
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&format!("file.{ex}"))
                .to_owned();

            Ok(SelectedMedia {
                from: MediaFrom::PathBuf(path),
                file_name,
                media_type,
            })
        } else {
            Err(Error::Generic(format!(
                "{path:?} does not have an extension"
            )))
        }
    }

    pub fn from_bytes(file_name: String, content: Vec<u8>) -> Result<Self, Error> {
        if let Some(ex) = PathBuf::from_str(&file_name)
            .unwrap()
            .extension()
            .and_then(|f| f.to_str())
        {
            let media_type = SupportedMimeType::from_extension(ex)?;

            Ok(SelectedMedia {
                from: MediaFrom::Memory(content),
                file_name,
                media_type,
            })
        } else {
            Err(Error::Generic(format!(
                "{file_name:?} does not have an extension"
            )))
        }
    }
}

pub struct SelectedMediaChannel {
    sender: Sender<Result<SelectedMedia, Error>>,
    receiver: Receiver<Result<SelectedMedia, Error>>,
}

impl Default for SelectedMediaChannel {
    fn default() -> Self {
        let (sender, receiver) = unbounded();
        Self { sender, receiver }
    }
}

impl SelectedMediaChannel {
    pub fn new_selected_file(&self, media: Result<SelectedMedia, Error>) {
        let _ = self.sender.send(media);
    }

    pub fn try_receive(&self) -> Option<Result<SelectedMedia, Error>> {
        self.receiver.try_recv().ok()
    }

    pub fn receive(&self) -> Option<Result<SelectedMedia, Error>> {
        self.receiver.recv().ok()
    }
}

pub static SELECTED_MEDIA_CHANNEL: Lazy<SelectedMediaChannel> =
    Lazy::new(SelectedMediaChannel::default);

pub fn emit_selected_file(media: Result<SelectedMedia, Error>) {
    SELECTED_MEDIA_CHANNEL.new_selected_file(media);
}

pub fn get_next_selected_file() -> Option<Result<SelectedMedia, Error>> {
    SELECTED_MEDIA_CHANNEL.try_receive()
}
