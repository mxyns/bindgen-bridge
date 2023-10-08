use crate::Result;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::fs::File;
use std::io::Read;
use std::path::PathBuf;
use std::str::FromStr;
use toml_edit::{table, Document, Formatted, Item, Table, Value};

pub type BindingsMap = phf::Map<&'static str, &'static str>;

#[derive(Debug, Clone, Copy, Eq, Ord, PartialOrd, PartialEq)]
pub enum TemplateError {
    MissingBindings,
    DocumentNotRead,
    InvalidSourcePath,
}

impl Display for TemplateError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let str = match self {
            TemplateError::MissingBindings => "Template is missing bindings",
            TemplateError::DocumentNotRead => "Template was not read before its use",
            TemplateError::InvalidSourcePath => "Template has an invalid source path",
        };

        write!(f, "{}", str)
    }
}

impl Error for TemplateError {}

pub struct Template<'bindings> {
    path: PathBuf,
    doc: Option<Document>,
    bindings: Option<&'bindings BindingsMap>,
}

impl<'template> Template<'template> {
    pub fn load_template<P: Into<PathBuf>>(path: P) -> Self {
        Self {
            path: path.into(),
            doc: None,
            bindings: None,
        }
    }

    pub fn read_as_toml(mut self) -> Result<Self> {
        let mut file = File::open(&self.path)?;
        let mut content = if let Ok(metadata) = file.metadata() {
            String::with_capacity(metadata.len() as usize)
        } else {
            String::new()
        };

        file.read_to_string(&mut content)?;
        self.doc = Some(Document::from_str(&content)?);

        Ok(self)
    }

    pub fn use_document(mut self, document: Document) -> Result<Self> {
        self.doc = Some(document);
        Ok(self)
    }

    pub fn with_bindings<'bindings: 'template>(mut self, map: &'bindings BindingsMap) -> Self {
        self.bindings = Some(map);
        self
    }

    pub fn generate_toml(&self) -> Result<Document> {
        if self.bindings.is_none() {
            return Err(Box::new(TemplateError::MissingBindings));
        }

        if self.doc.is_none() {
            return Err(Box::new(TemplateError::DocumentNotRead));
        }

        let mut document = self.doc.clone().unwrap();

        let mut renames = if let Some(table) = document.get_mut("export.rename") {
            table.as_table_mut().unwrap()
        } else {
            document["export"]["rename"] = table();
            document["export"]["rename"].as_table_mut().unwrap()
        };

        let bindings = self.bindings.unwrap();
        extend_toml_table_with_bindings_map(&mut renames, bindings);

        Ok(document)
    }

    pub fn config_header(&self) -> Result<String> {
        if let Some(path) = self.path.to_str() {
            Ok(format!(
                "# This configuration file has been automatically generated\n\
    # Do not modify it manually. Instead, make changes to its associated template : {path}\n\n",
            ))
        } else {
            Err(Box::new(TemplateError::InvalidSourcePath))
        }
    }
}

fn extend_toml_table_with_bindings_map(table: &mut Table, map: &BindingsMap) {

    map.into_iter().for_each(|(rust_name, c_name)| {
        // need this to escape the string quotes
        let c_name_text = c_name.to_string();
        table.insert(
            &rust_name,
            Item::Value(Value::String(Formatted::new(c_name_text))),
        );
    });
}

#[cfg(test)]
mod tests {
    use crate::export::{BindingsMap, extend_toml_table_with_bindings_map};
    use phf_macros::phf_map;

    #[test]
    fn convert_map() {
        let map: BindingsMap = phf_map! {
            "bmp_common_hdr" => "struct bmp_common_hdr",
            "bmp_peer_hdr" => "struct bmp_peer_hdr",
        };

        let mut converted = toml_edit::Table::new();
        extend_toml_table_with_bindings_map(&mut converted, &map);

        assert_eq!(converted.to_string(),
                   String::from("bmp_peer_hdr = \"struct bmp_peer_hdr\"\nbmp_common_hdr = \"struct bmp_common_hdr\"\n"))
    }
}
