use std::{collections::HashMap, fs, path::PathBuf, time::SystemTime};

use crate::Error;

pub struct FileWriterFactory {
    writer_type: FileWriterType,
    use_test_dir: Option<fn() -> Result<PathBuf, Error>>,
}

impl FileWriterFactory {
    pub fn new(writer_type: FileWriterType) -> Self {
        Self {
            writer_type,
            use_test_dir: None,
        }
    }

    pub fn testing_with(mut self, test_dir: fn() -> Result<PathBuf, Error>) -> Self {
        self.use_test_dir = Some(test_dir);
        self
    }

    pub fn build(self) -> Result<FileDirectoryInteractor, Error> {
        let file_path = if let Some(create_dir) = self.use_test_dir {
            create_dir()?
        } else {
            self.writer_type.get_path("notedeck")?
        };

        Ok(FileDirectoryInteractor { file_path })
    }
}

pub enum FileWriterType {
    Log,
    Setting,
    Keys,
    SelectedKey,
}

impl FileWriterType {
    pub fn get_path(&self, app_name: &str) -> Result<PathBuf, Error> {
        let base_path = match self {
            FileWriterType::Log => dirs::data_local_dir(),
            FileWriterType::Setting | FileWriterType::Keys | FileWriterType::SelectedKey => {
                dirs::config_local_dir()
            }
        }
        .ok_or(Error::Generic(
            "Could not open well known OS directory".to_owned(),
        ))?;

        let specific_path = match self {
            FileWriterType::Log => PathBuf::from("logs"),
            FileWriterType::Setting => PathBuf::from("settings"),
            FileWriterType::Keys => PathBuf::from("storage").join("accounts"),
            FileWriterType::SelectedKey => PathBuf::from("storage").join("selected_account"),
        };

        Ok(base_path.join(app_name).join(specific_path))
    }
}

#[derive(Debug, PartialEq)]
pub struct FileDirectoryInteractor {
    file_path: PathBuf,
}

impl FileDirectoryInteractor {
    /// Write the file to the `file_path` directory
    pub fn write(&self, file_name: String, data: &str) -> Result<(), Error> {
        if !self.file_path.exists() {
            fs::create_dir_all(self.file_path.clone())?
        }

        std::fs::write(self.file_path.join(file_name), data)?;
        Ok(())
    }

    /// Get the files in the current directory where the key is the file name and the value is the file contents
    pub fn get_files(&self) -> Result<HashMap<String, String>, Error> {
        let dir = fs::read_dir(self.file_path.clone())?;
        let map = dir
            .filter_map(|f| f.ok())
            .filter(|f| f.path().is_file())
            .filter_map(|f| {
                let file_name = f.file_name().into_string().ok()?;
                let contents = fs::read_to_string(f.path()).ok()?;
                Some((file_name, contents))
            })
            .collect();

        Ok(map)
    }

    pub fn get_file_names(&self) -> Result<Vec<String>, Error> {
        let dir = fs::read_dir(self.file_path.clone())?;
        let names = dir
            .filter_map(|f| f.ok())
            .filter(|f| f.path().is_file())
            .filter_map(|f| f.file_name().into_string().ok())
            .collect();

        Ok(names)
    }

    pub fn get_file(&self, file_name: String) -> Result<String, Error> {
        let filepath = self.file_path.clone().join(file_name.clone());

        if filepath.exists() && filepath.is_file() {
            let filepath_str = filepath
                .to_str()
                .ok_or_else(|| Error::Generic("Could not turn path to string".to_owned()))?;
            Ok(fs::read_to_string(filepath_str)?)
        } else {
            Err(Error::Generic(format!(
                "Requested file was not found: {}",
                file_name
            )))
        }
    }

    pub fn delete_file(&self, file_name: String) -> Result<(), Error> {
        let file_to_delete = self.file_path.join(file_name.clone());
        if file_to_delete.exists() && file_to_delete.is_file() {
            fs::remove_file(file_to_delete).map_err(Error::Io)
        } else {
            Err(Error::Generic(format!(
                "Requested file to delete was not found: {}",
                file_name
            )))
        }
    }

    pub fn get_directory(&self) -> &PathBuf {
        &self.file_path
    }

    /// Get the file name which is most recently modified in the directory
    pub fn get_most_recent(&self) -> Result<Option<String>, Error> {
        let mut most_recent: Option<(SystemTime, String)> = None;

        for entry in fs::read_dir(&self.file_path)? {
            let entry = entry?;
            let metadata = entry.metadata()?;
            if metadata.is_file() {
                let modified = metadata.modified()?;
                let file_name = entry.file_name().to_string_lossy().to_string();

                match most_recent {
                    Some((last_modified, _)) if modified > last_modified => {
                        most_recent = Some((modified, file_name));
                    }
                    None => {
                        most_recent = Some((modified, file_name));
                    }
                    _ => {}
                }
            }
        }

        Ok(most_recent.map(|(_, file_name)| file_name))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use crate::Error;

    use super::FileWriterFactory;

    static CREATE_TMP_DIR: fn() -> Result<PathBuf, Error> =
        || Ok(tempfile::TempDir::new()?.path().to_path_buf());

    #[test]
    fn test_add_get_delete() {
        if let Ok(interactor) = FileWriterFactory::new(super::FileWriterType::Keys)
            .testing_with(CREATE_TMP_DIR)
            .build()
        {
            let file_name = "file_test_name.txt".to_string();
            let file_contents = "test";
            let write_res = interactor.write(file_name.clone(), file_contents);
            assert!(write_res.is_ok());

            if let Ok(asserted_file_contents) = interactor.get_file(file_name.clone()) {
                assert_eq!(asserted_file_contents, file_contents);
            } else {
                panic!("File not found");
            }

            let delete_res = interactor.delete_file(file_name);
            assert!(delete_res.is_ok());
        } else {
            panic!("could not get interactor")
        }
    }

    #[test]
    fn test_get_multiple() {
        if let Ok(interactor) = FileWriterFactory::new(super::FileWriterType::Keys)
            .testing_with(CREATE_TMP_DIR)
            .build()
        {
            for i in 0..10 {
                let file_name = format!("file{}.txt", i);
                let write_res = interactor.write(file_name, "test");
                assert!(write_res.is_ok());
            }

            if let Ok(files) = interactor.get_files() {
                for i in 0..10 {
                    let file_name = format!("file{}.txt", i);
                    assert!(files.contains_key(&file_name));
                    assert_eq!(files.get(&file_name).unwrap(), "test");
                }
            } else {
                panic!("Files not found");
            }

            if let Ok(file_names) = interactor.get_file_names() {
                for i in 0..10 {
                    let file_name = format!("file{}.txt", i);
                    assert!(file_names.contains(&file_name));
                }
            } else {
                panic!("File names not found");
            }

            for i in 0..10 {
                let file_name = format!("file{}.txt", i);
                assert!(interactor.delete_file(file_name).is_ok());
            }
        } else {
            panic!("could not get interactor")
        }
    }
}
