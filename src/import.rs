use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::fmt::Write;
use std::rc::Rc;

use bindgen::callbacks::{DiscoveredItem, DiscoveredItemId};
use phf_codegen::Map;
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::Result;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CName {
    /// The identifier used to address a type
    pub identifier: String,

    /// Whether the name of [CName::identifier] is an alias or not
    pub aliased: bool,
}

#[derive(Clone, Copy, Debug, Ord, PartialOrd, PartialEq, Eq)]
pub enum CompositeKind {
    Struct,
    Union,
}

impl TryFrom<&DiscoveredItem> for CompositeKind {
    type Error = ();

    fn try_from(value: &DiscoveredItem) -> std::result::Result<Self, Self::Error> {
        match value {
            DiscoveredItem::Struct { .. } => Ok(Self::Struct),
            DiscoveredItem::Union { .. } => Ok(Self::Union),
            DiscoveredItem::Alias { .. } => Err(())
        }
    }
}

/// One mapping between a type's C name, Rust name, and C aliases
#[derive(Debug, Eq, PartialEq, Clone)]
pub struct NameMapping {
    /// The kind of composite type (struct or union)
    pub kind: CompositeKind,

    /// Name of the imported type from C
    /// This is optional because of anonymous types
    pub c_name: Option<CName>,

    /// Name of the type in Rust after import by bindgen
    pub rust_name: String,

    /// List of known aliases for the type
    pub aliases: BTreeSet<String>,
}

impl NameMapping {
    /// Figures out the original name in the C code based on the type and its name
    ///
    /// the name of a struct named A is "struct A"
    /// the name of an union named B is "union A"
    ///
    /// If the passed name is an alias, keep it that way
    pub fn validated_original_name(c_name: Option<&CName>, kind: CompositeKind) -> Option<String> {
        let original_name = &c_name?.identifier;

        // has a space because we use it to ensure it is not yet present in the name
        let prefix = match kind {
            CompositeKind::Struct => "struct ",
            CompositeKind::Union => "enum ",
        };

        // do not prepend the prefix to an aliased type
        let result = if c_name?.aliased || original_name.starts_with(prefix) {
            original_name.clone()
        } else {
            format!("{prefix}{original_name}")
        };

        Some(result)
    }
}

#[derive(Debug, Default, Clone, Eq, PartialEq)]
pub struct NameMappings {
    /// The discovered types and their mappings
    pub types: HashMap<DiscoveredItemId, NameMapping>,

    /// The known aliases without an associated type mappings
    pub aliases: HashMap<DiscoveredItemId, BTreeSet<String>>,
}

impl NameMappings {
    /// Drain the temporary alias cache
    pub fn forget_unused_aliases(&mut self) -> usize {
        self.aliases.drain().map(|(_, set)| set.len()).sum()
    }

    /// Generate a cbindgen.toml [export.rename] section, without the section header
    pub fn to_cbindgen_toml_renames(&self, force_aliases_use: bool) -> Result<String> {
        let mut result = String::with_capacity(self.types.len() * 16); // rough approximate of the capacity

        for (id, mapping) in &self.types {
            let use_name =
                if mapping.c_name.is_none() || (force_aliases_use && !mapping.aliases.is_empty()) {
                    mapping.aliases.iter().next().cloned()
                } else {
                    NameMapping::validated_original_name(mapping.c_name.as_ref(), mapping.kind)
                };

            if let Some(use_name) = use_name {
                writeln!(&mut result, "\"{}\" = \"{}\"", mapping.rust_name, use_name)?;
            } else {
                eprintln!(
                    "Warn: type with no valid name during rename export! id={:#?} info={:#?}",
                    id, mapping
                );
                continue;
            }
        }

        Ok(result)
    }

    /// Generates a [phf_codegen] static map from the mappings
    ///
    /// Uses the first alias given by the [NameMappings::aliases]'s [BTreeSet] values for the rename rule
    /// (no guarantee on which one, but it's likely based on Strings' alphabetical ordering)
    pub fn to_static_map(&self, force_aliases_use: bool) -> Result<Map<String>> {
        let mut result = Map::new();

        for (id, mapping) in &self.types {
            let use_name =
                if mapping.c_name.is_none() || (force_aliases_use && !mapping.aliases.is_empty()) {
                    mapping.aliases.iter().next().cloned()
                } else {
                    NameMapping::validated_original_name(mapping.c_name.as_ref(), mapping.kind)
                };

            if let Some(use_name) = use_name {
                result.entry(mapping.rust_name.clone(), &format!("\"{}\"", use_name));
            } else {
                eprintln!(
                    "Warn: type with no valid name during rename export! id={:#?} info={:#?}",
                    id, mapping
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

// callback behaviour pseudo code
// types: Map ItemId => Info { canonical_ident (final rust name), original_name(item.kind.type.name), HashSetAlias> }
// found_aliases: Map ItemId => Alias
//
// on new type/item: call new composite callback => insert to map, check found_aliases
// on new alias: call new alias callback => if alias.type in types types.get(alias.type.id).push_alias(alias) else found_aliases.push(alias)
// on resolvedtyperef: call new alias callback => ^ + typeref.name != original_name
impl bindgen::callbacks::ParseCallbacks for NameMappingsCallback {
    fn new_item_found(&self, id: DiscoveredItemId, item: DiscoveredItem) {
        match &item {
            DiscoveredItem::Struct { original_name, final_name }
            | DiscoveredItem::Union { original_name, final_name } => {
                self.new_composite_found(id, CompositeKind::try_from(&item).unwrap(), original_name.as_ref().map(String::as_str), final_name)
            }
            DiscoveredItem::Alias { alias_name, alias_for } => {
                self.new_alias_found(id, alias_name, *alias_for)
            }
        }
    }
}

impl NameMappingsCallback {
    /// Called when a new composite type is found (struct / union)
    ///
    /// Saves the type, its name, its aliases
    fn new_composite_found(
        &self,
        id: DiscoveredItemId,
        kind: CompositeKind,
        original_name: Option<&str>,
        final_ident: &str,
    ) {
        let mut mappings = self.0.borrow_mut();

        let mut aliases = mappings
            .aliases
            .remove(&id)
            .unwrap_or_else(|| BTreeSet::new());

        // if the struct is not anonymous
        let c_name = if original_name.is_some() {
            // build a non-aliased CName since we know the type's actual name
            let c_name = original_name.map(|name| CName {
                identifier: name.to_string(),
                aliased: false,
            });

            // remove all aliases with the same name (including the type keyword)
            // this takes out "struct my_struct" while keeping "my_struct" as an alias for the
            // typedef struct my_struct {..} my_struct; pattern
            if let Some(original_name) = NameMapping::validated_original_name(c_name.as_ref(), kind)
            {
                aliases.retain(|value| !value.eq(&original_name));
            }

            c_name
        }
        // if the struct is anonymous and we already know an alias for it
        // we use use the latter as the new name, but remember that it was aliased
        else if let Some(one_alias) = aliases.iter().next().cloned() {
            aliases.take(&one_alias).map(|name| CName {
                identifier: name,
                aliased: true,
            })
            // for an unknown anonymous struct without aliases we can't invent a name
        } else {
            None
        };

        println!(
            "kind : {:?} original {:?} => {:?}",
            kind, original_name, c_name
        );

        if let Some(duplicate) = mappings.types.insert(
            id,
            NameMapping {
                kind,
                c_name: c_name.clone(), // may still be unknown in case of anonymous struct without known aliases
                rust_name: final_ident.to_string(),
                aliases,
            },
        ) {
            println!(
                "Warn: duplicated definition for {{ id={:?} name={:?} }}! previous: {:?}",
                id, c_name, duplicate
            )
        }
    }

    /// Called when a new alias is found
    ///
    /// Saves the alias either as an alias or the base name (if none is known yet) for known types.
    /// The alias is saved for later when the type is not known yet
    fn new_alias_found(&self, _id: DiscoveredItemId, alias_name: &str, target_id: DiscoveredItemId) {
        let mut mappings = self.0.borrow_mut();

        let aliased_name = alias_name.to_string();

        if let Some(mapping) = mappings.types.get_mut(&target_id) {
            // if the structure was anonymous let's use one of its aliases as a name
            if let None = mapping.c_name {
                mapping.c_name = Some(CName {
                    identifier: aliased_name,
                    aliased: true,
                });
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

    /// Always promote an alias to be the C name of a struct if possible.
    /// e.g.: `struct MyStruct` with a `typedef struct MyStruct AliasOfMyStruct` will be known as `AliasOfMyStruct`
    /// see [MappingsCodegen::force_aliases_use]
    force_aliases_use: bool,

    /// see [MappingsCodegen::as_static_map]
    as_static_map: bool,

    /// see [MappingsCodegen::variable_name]
    variable_name: Option<&'var_name str>,
}

impl From<NameMappings> for MappingsCodegen<'_> {
    fn from(value: NameMappings) -> Self {
        Self {
            mappings: value,
            force_aliases_use: false,
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

    /// Should we use the first (by [`BTreeSet<String>`] ordering) known alias of the types
    /// as the C name of the types
    ///
    /// default: false
    pub fn force_aliases_use(&mut self, yes: bool) -> &mut Self {
        self.force_aliases_use = yes;
        self
    }

    /// Should we export the code as a [Map]
    /// if `false` (by default) the code generated is a static raw str in a toml format
    /// without the section header to let you use it where you want
    ///
    /// default: false
    pub fn as_static_map(&mut self, yes: bool) -> &mut Self {
        self.as_static_map = yes;
        self
    }

    /// Name of the static variable used to store the exported value in the generated code
    /// If `None`, the generated code will just be the value, without a variable assignment
    ///
    /// default: None
    pub fn variable_name(&mut self, variable_name: Option<&'var_name str>) -> &mut Self {
        self.variable_name = if variable_name.is_some() && variable_name.unwrap() == "" {
            None
        } else {
            variable_name
        };

        self
    }

    /// Generate a [TokenStream] based on all the parameters set on [Self]
    pub fn generate(&self) -> Result<TokenStream> {
        let variable_name_ident = self.variable_name.map(|name| format_ident!("{}", name));

        let var_type = if self.as_static_map {
            quote! {
                phf::Map<&'static str, &'static str>
            }
        } else {
            quote! {
                &'static str
            }
        };

        let mut value = if self.as_static_map {
            self.mappings
                .to_static_map(self.force_aliases_use)?
                .build()
                .to_string()
        } else {
            format!("\"{}\"", self.mappings.to_cbindgen_toml_renames(self.force_aliases_use)?.replace('"', "\\\""))
        }
            .parse::<TokenStream>()?;

        if let Some(bindings_name) = variable_name_ident {
            value = quote! {
                pub static #bindings_name : #var_type = #value;
            };
        }

        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::{BTreeSet, HashMap};
    use std::rc::Rc;

    use bindgen::Builder;
    use bindgen::callbacks::DiscoveredItemId;

    use crate::import::{CName, NameMapping, NameMappings, NameMappingsCallback};
    use crate::import::CompositeKind::{Struct, Union};

    #[test]
    fn pass() {}

    #[test]
    fn test_mappings() {

        let mappings = Rc::new(RefCell::new(NameMappings::default()));
        Builder::default()
            .header_contents("sample_header.h","
                // Unions
                void function_using_anonymous_struct(struct {} arg0);

                struct NamedStruct {
                };

                typedef struct NamedStruct AliasOfNamedStruct;


                // Unions
                void function_using_anonymous_union(union {} arg0);

                union NamedUnion {
                };

                typedef union NamedUnion AliasOfNamedUnion;
        ")
            .parse_callbacks(Box::new(NameMappingsCallback(Rc::clone(&mappings))))
            .generate()
            .unwrap();

        let expected =  NameMappings {
            types: HashMap::from([
                (DiscoveredItemId::new(1),
                 NameMapping {
                    kind: Struct,
                    c_name: None,
                    rust_name: "_bindgen_ty_1".to_string(),
                    aliases: BTreeSet::default(),
                }),
                (DiscoveredItemId::new(10),
                 NameMapping {
                    kind: Union,
                    c_name: None,
                    rust_name: "_bindgen_ty_2".to_string(),
                    aliases: BTreeSet::default(),
                }),
                (DiscoveredItemId::new(16),
                 NameMapping {
                    kind: Union,
                    c_name: Some(
                        CName {
                            identifier: "NamedUnion".to_string(),
                            aliased: false,
                        },
                    ),
                    rust_name: "NamedUnion".to_string(),
                    aliases: BTreeSet::from(["AliasOfNamedUnion".to_string()])
                }),
                (DiscoveredItemId::new(7),
                    NameMapping {
                    kind: Struct,
                    c_name: Some(
                        CName {
                            identifier: "NamedStruct".to_string(),
                            aliased: false,
                        },
                    ),
                    rust_name: "NamedStruct".to_string(),
                    aliases: BTreeSet::from(["AliasOfNamedStruct".to_string()])
                })
            ]),
            aliases: HashMap::default(),
        };
        
        assert!(expected.eq(&mappings.borrow()));
    }

    #[test]
    fn codegen() {
        let mappings = NameMappings::default();

        let code = mappings
            .codegen()
            .variable_name(Some("super_var"))
            .generate()
            .unwrap();

        assert_eq!(
            code.to_string(),
            "pub static super_var : & 'static str = \"\" ;"
        )
    }
}
