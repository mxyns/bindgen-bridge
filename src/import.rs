use crate::Result;
use bindgen::CompKind;
use phf_codegen::Map;
use proc_macro2::{Ident, TokenStream};
use quote::quote;
use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::fmt::Write;
use std::rc::Rc;

/// Discovered Type ID
#[derive(Debug, Default, Ord, PartialOrd, Eq, PartialEq, Hash, Clone, Copy)]
#[repr(transparent)]
pub struct Type(usize);

/// One mapping between a type's C name, Rust name, and C aliases
#[derive(Debug, Default, Eq, PartialEq, Clone)]
pub struct NameMapping {
    /// Name of the imported type from C
    /// This is optional because of anonymous types
    c_name: Option<String>,

    /// Name of the type in Rust after import by bindgen
    rust_name: String,

    /// List of known aliases for the type
    aliases: BTreeSet<String>,
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

#[derive(Debug, Default, Clone)]
pub struct NameMappings {
    /// The discovered types and their mappings
    types: HashMap<Type, NameMapping>,

    /// The known aliases without an associated type mappings
    aliases: HashMap<Type, BTreeSet<String>>,
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

    /// Generates a [phf_codegen] static map from the mappings
    ///
    /// Uses the first alias given by the [NameMappings::aliases]'s BTreeSet values for the rename rule
    /// (no guarantee on which one, but it's likely be based on Strings' alphabetical ordering)
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

    /// Wraps these mappings in a [MappingsCodegen] builder to export the mappings as static code
    ///
    /// Reversible with [MappingsCodegen::mappings]
    pub fn codegen<'a>(self) -> MappingsCodegen<'a> {
        self.into()
    }
}

/// The callback to include with [bindgen::Builder::parse_callbacks] in your `build.rs`
/// to discover types and aliases during the C header parsing.
#[derive(Debug)]
pub struct NameMappingsCallback(pub Rc<RefCell<NameMappings>>);

/// callback behaviour pseudo code
/// types: Map ItemId => Info { canonical_ident (final rust name), original_name(item.kind.type.name), HashSet<Alias> }
/// found_aliases: Map ItemId => Alias
///
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
            .unwrap_or_else(|| BTreeSet::new());

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

/// Code builder used to export mappings by generating [TokenStream]s
#[derive(Debug, Clone)]
pub struct MappingsCodegen<'var_name> {
    /// Mappings used to generate code
    mappings: NameMappings,

    /// see [MappingsCodegen::use_aliases]
    use_aliases: bool,

    /// see [MappingsCodegen::as_static_map]
    as_static_map: bool,

    /// see [MappingsCodegen::variable_name]
    variable_name: Option<&'var_name str>,
}

impl From<NameMappings> for MappingsCodegen<'_> {
    fn from(value: NameMappings) -> Self {
        Self {
            mappings: value,
            use_aliases: false,
            as_static_map: false,
            variable_name: None,
        }
    }
}

impl From<MappingsCodegen<'_>> for NameMappings {
    fn from(value: MappingsCodegen) -> Self {
        value.mappings
    }
}

impl<'var_name> MappingsCodegen<'var_name> {
    /// Unwrap back into a [NameMappings], loses the settings of the [MappingsCodegen]
    pub fn mappings(self) -> NameMappings {
        self.into()
    }

    /// Should we use the first (by [BTreeSet<String>] ordering) known alias of the types
    /// as the C name used
    ///
    /// default: false
    pub fn use_aliases(mut self, will: bool) -> Self {
        self.use_aliases = will;
        self
    }

    /// Should we export the code as a [Map]
    /// if `false` (by default) the code generated is a static raw str in a toml format
    /// without the section header to let you use it where you want
    ///
    /// default: false
    pub fn as_static_map(mut self, will: bool) -> Self {
        self.as_static_map = will;
        self
    }

    /// Name of the static variable used to store the exported value in the generated code
    /// If `None`, the generated code will just be the value, without a variable assignment
    ///
    /// default: None
    pub fn variable_name(mut self, variable_name: Option<&'var_name str>) -> Self {
        self.variable_name = if variable_name.is_some() && variable_name.unwrap() == "" {
            None
        } else {
            variable_name
        };

        self
    }

    /// Generate a [TokenStream] based on all the parameters set on [Self]
    pub fn generate(self) -> Result<TokenStream> {
        let quote = if self.as_static_map {
            let map: TokenStream = self
                .mappings
                .to_static_map(self.use_aliases)?
                .build()
                .to_string()
                .parse()?;

            if let Some(bindings_name) = self.variable_name {
                quote! {
                    pub static #bindings_name : phf::Map<&'static str, &'static str> = #map;
                }
            } else {
                quote! {
                    #map
                }
            }
        } else {
            let toml = self.mappings.to_cbindgen_toml_renames(self.use_aliases)?;

            if let Some(bindings_name) = self.variable_name {
                quote! {
                    pub static #bindings_name : &'static str = #toml;
                }
            } else {
                quote! {
                    #toml
                }
            }
        };

        Ok(quote)
    }
}

mod tests {
    #[test]
    fn pass() {}
}
