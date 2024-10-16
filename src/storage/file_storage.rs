use std::{collections::HashMap, fs, io, path::PathBuf};

use crate::Error;

pub struct FileWriterFactory {
    writer_type: FileWriterType,
    is_test: bool,
}

impl FileWriterFactory {
    pub fn new(writer_type: FileWriterType) -> Self {
        Self {
            writer_type,
            is_test: false,
        }
    }

    pub fn testing(mut self) -> Self {
        self.is_test = true;
        self
    }

    pub fn build(self) -> Result<FileDirectoryInteractor, Error> {
        let app_name = if self.is_test {
            "notedeck_test"
        } else {
            "notedeck"
        };
        let file_path = self.writer_type.get_path(app_name)?;

        Ok(FileDirectoryInteractor { file_path })
    }
}

pub enum SupportedTargets {
    MacOS,
    Linux,
}

impl SupportedTargets {
    pub fn current() -> Option<SupportedTargets> {
        if cfg!(target_os = "macos") {
            Some(SupportedTargets::MacOS)
        } else if cfg!(target_os = "linux") {
            Some(SupportedTargets::Linux)
        } else {
            None
        }
    }
}

pub enum FileWriterType {
    Log,
    Setting,
    Keys,
    SelectedKey,
}

impl FileWriterType {
    pub fn get_path(&self, app_name: &str) -> Result<PathBuf, crate::Error> {
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
            FileWriterType::Log => PathBuf::new(),
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
    pub fn write(&self, file_name: String, data: &str) -> Result<(), io::Error> {
        if !self.file_path.exists() {
            fs::create_dir_all(self.file_path.clone())?
        }

        std::fs::write(self.file_path.join(file_name), data)?;
        Ok(())
    }

    /// Get the files in the current directory where the key is the file name and the value is the file contents
    pub fn get_files(&self) -> Result<HashMap<String, String>, io::Error> {
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

    pub fn get_file_names(&self) -> Result<Vec<String>, io::Error> {
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
            fs::remove_file(file_to_delete).map_err(|e| Error::Io(e))
        } else {
            Err(Error::Generic(format!(
                "Requested file to delete was not found: {}",
                file_name
            )))
        }
    }
}

mod tests {
    use core::panic;

    use crate::storage::file_storage::FileWriterFactory;

    #[test]
    fn test_add_get_delete() {
        if let Ok(interactor) = FileWriterFactory::new(super::FileWriterType::Keys)
            .testing()
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
            .testing()
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
