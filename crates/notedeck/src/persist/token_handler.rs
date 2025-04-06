use tokenator::{ParseError, ParseErrorOwned, TokenParser, TokenSerializable, TokenWriter};

use crate::{storage, DataPath, DataPathType, Directory};

pub struct TokenHandler {
    directory: Directory,
    file_name: &'static str,
}

impl TokenHandler {
    pub fn new(path: &DataPath, path_type: DataPathType, file_name: &'static str) -> Self {
        let directory = Directory::new(path.path(path_type));

        Self {
            directory,
            file_name,
        }
    }

    pub fn save(
        &self,
        tokenator: &impl TokenSerializable,
        delim: &'static str,
    ) -> crate::Result<()> {
        let mut writer = TokenWriter::new(delim);

        tokenator.serialize_tokens(&mut writer);
        let to_write = writer.str();

        storage::write_file(
            &self.directory.file_path,
            self.file_name.to_owned(),
            to_write,
        )
    }

    pub fn load<T: TokenSerializable>(
        &self,
        delim: &'static str,
    ) -> crate::Result<Result<T, ParseErrorOwned>> {
        match self.directory.get_file(self.file_name.to_owned()) {
            Ok(s) => {
                let data = s.split(delim).collect::<Vec<&str>>();
                let mut parser = TokenParser::new(&data);
                Ok(TokenSerializable::parse_from_tokens(&mut parser).map_err(ParseError::into))
            }
            Err(e) => Err(e),
        }
    }

    pub fn clear(&self) -> crate::Result<()> {
        storage::write_file(&self.directory.file_path, self.file_name.to_owned(), "")
    }
}
