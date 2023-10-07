use crate::Result;
use bindgen::CompKind;
use phf_codegen::Map;
use proc_macro2::Ident;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::rc::Rc;

#[derive(Debug, Default, Ord, PartialOrd, Eq, PartialEq, Hash)]
#[repr(transparent)]
pub struct Type(usize);

#[derive(Debug, Default, Eq, PartialEq)]
pub struct NameMapping {
    /// Name of the imported type from C
    /// This is optional because of anonymous types
    c_name: Option<String>,

    /// Name of the type in Rust after import by bindgen
    rust_name: String,

    /// List of known aliases for the type
    aliases: HashSet<String>,
}

impl NameMapping {
    /// Figures out the original name in the C code based on the type and its name
    ///
    /// the name of a struct named A is "struct A"
    /// the name of an union named B is "union A"
    pub fn build_c_name(kind: CompKind, original_name: Option<&str>) -> Option<String> {
        let original_name = original_name?;

        // has a space
        let prefix = match kind {
            CompKind::Struct => "struct ",
            CompKind::Union => "enum ",
        };

        let result = if original_name.starts_with(prefix) {
            original_name.to_string()
        } else {
            format!("{prefix}{original_name}")
        };

        Some(result)
    }
}

#[derive(Debug, Default)]
pub struct NameMappings {
    types: HashMap<Type, NameMapping>,
    aliases: HashMap<Type, HashSet<String>>,
}

impl NameMappings {
    /// Drain the temporary alias cache
    pub fn forget_unused_aliases(&mut self) -> usize {
        self.aliases.drain().map(|(_, set)| set.len()).sum()
    }

    /// Generate a cbindgen.toml [export.rename] section, without the section header
    pub fn to_cbindgen_toml_renames(&self, use_aliases: bool) -> Result<String> {
        let mut result = String::with_capacity(self.types.len() * 16); // rough approximate of the capacity

        for (id, mapping) in &self.types {
            let use_name =
                if mapping.c_name.is_none() || (use_aliases && !mapping.aliases.is_empty()) {
                    mapping.aliases.iter().next()
                } else {
                    mapping.c_name.as_ref()
                };

            if let Some(use_name) = use_name {
                writeln!(&mut result, "\"{}\" = \"{}\"", mapping.rust_name, use_name)?;
            } else {
                eprintln!(
                    "Warn: type with no valid name during rename export! id={} info={:#?}",
                    id.0, mapping
                );
                continue;
            }
        }

        Ok(result)
    }

    pub fn to_static_map(&self, use_aliases: bool) -> Result<Map<String>> {
        let mut result = Map::new();

        for (id, mapping) in &self.types {
            let use_name =
                if mapping.c_name.is_none() || (use_aliases && !mapping.aliases.is_empty()) {
                    mapping.aliases.iter().next()
                } else {
                    mapping.c_name.as_ref()
                };

            if let Some(use_name) = use_name {
                result.entry(mapping.rust_name.clone(), &format!("\"{}\"", use_name));
            } else {
                eprintln!(
                    "Warn: type with no valid name during rename export! id={} info={:#?}",
                    id.0, mapping
                );
                continue;
            }
        }

        Ok(result)
    }
}

#[derive(Debug)]
pub struct NameMappingsCallback(pub Rc<RefCell<NameMappings>>);

/// types: Map ItemId => Info { canonical_ident (final rust name), original_name(item.kind.type.name), HashSet<Alias> }
/// found_aliases: Map ItemId => Alias
/// on new type/item: call new composite callback => insert to map, check found_aliases
/// on new alias: call new alias callback => if alias.type in types types.get(alias.type.id).push_alias(alias) else found_aliases.push(alias)
/// on resolvedtyperef: call new alias callback => ^ + typeref.name != original_name
impl bindgen::callbacks::ParseCallbacks for NameMappingsCallback {
    /// Called when a new composite type is found (struct / union)
    ///
    /// Saves the type, its name, its aliases
    fn new_composite_found(
        &self,
        _id: usize,
        _kind: CompKind,
        _original_name: Option<&str>,
        _final_ident: &Ident,
    ) {
        let mut mappings = self.0.borrow_mut();

        let id = Type(_id);
        let mut aliases = mappings
            .aliases
            .remove(&id)
            .unwrap_or_else(|| HashSet::new());

        let mut c_name = NameMapping::build_c_name(_kind, _original_name);

        println!(
            "kind : {:?} original {:?} => {:?}",
            _kind, _original_name, c_name
        );
        // if the struct is not anonymous, remove all aliases with the same name
        if c_name.is_some() {
            aliases.retain(|value| !value.eq(c_name.as_deref().unwrap()));
        }
        // if the struct is anonymous we use one of the already known aliases as a name for it
        else if let Some(one_alias) = aliases.iter().next().cloned() {
            c_name = aliases.take(&one_alias)
        }

        if let Some(duplicate) = mappings.types.insert(
            id,
            NameMapping {
                c_name: c_name.clone(), // may still be unknown in case of anonymous struct without known aliases
                rust_name: _final_ident.to_string(),
                aliases,
            },
        ) {
            println!(
                "Warn: duplicated definition for {{ id={} name={:?} }}! previous: {:?}",
                _id, c_name, duplicate
            )
        }
    }

    /// Called when a new alias is found
    ///
    /// Saves the alias either as an alias or the base name (if none is known yet) for known types
    /// The alias is saved for later when the type is not known yet
    fn new_alias_found(&self, _id: usize, _alias_name: &Ident, _alias_for: usize) {
        let mut mappings = self.0.borrow_mut();

        let target_id = Type(_alias_for);
        let aliased_name = _alias_name.to_string();

        if let Some(mapping) = mappings.types.get_mut(&target_id) {
            // if the structure was anonymous let's use one of its aliases as a name
            if let None = mapping.c_name {
                mapping.c_name = Some(aliased_name.clone());
            }
            // if it wasn't, remember the alias
            else {
                mapping.aliases.insert(aliased_name);
            }
        } else {
            mappings
                .aliases
                .entry(target_id)
                .or_default()
                .insert(aliased_name);
        };
    }
}

mod tests {
    #[test]
    fn pass() {}
}
