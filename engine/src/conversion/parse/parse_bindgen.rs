// Copyright 2020 Google LLC
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::collections::HashSet;

use crate::{
    conversion::{
        api::{Api, ApiName, NullPhase, StructDetails, SubclassName, TypedefKind, UnanalyzedApi},
        apivec::ApiVec,
        ConvertError,
    },
    types::Namespace,
    types::QualifiedName,
};
use crate::{
    conversion::{
        convert_error::{ConvertErrorWithContext, ErrorContext},
        error_reporter::report_any_error,
    },
    types::validate_ident_ok_for_cxx,
};
use autocxx_parser::IncludeCppConfig;
use syn::{parse_quote, Fields, Ident, Item, TypePath, UseTree};

use super::{
    super::utilities::generate_utilities, bindgen_semantic_attributes::BindgenSemanticAttributes,
};

use super::parse_foreign_mod::ParseForeignMod;

/// Parses a bindgen mod in order to understand the APIs within it.
pub(crate) struct ParseBindgen<'a> {
    config: &'a IncludeCppConfig,
    apis: ApiVec<NullPhase>,
}

fn api_name(ns: &Namespace, id: Ident, attrs: &BindgenSemanticAttributes) -> ApiName {
    ApiName::new_with_cpp_name(ns, id, attrs.get_original_name())
}

pub(crate) fn api_name_qualified(
    ns: &Namespace,
    id: Ident,
    attrs: &BindgenSemanticAttributes,
) -> Result<ApiName, ConvertErrorWithContext> {
    match validate_ident_ok_for_cxx(&id.to_string()) {
        Err(e) => {
            let ctx = ErrorContext::Item(id);
            Err(ConvertErrorWithContext(e, Some(ctx)))
        }
        Ok(..) => Ok(api_name(ns, id, attrs)),
    }
}

impl<'a> ParseBindgen<'a> {
    pub(crate) fn new(config: &'a IncludeCppConfig) -> Self {
        ParseBindgen {
            config,
            apis: ApiVec::new(),
        }
    }

    /// Parses items found in the `bindgen` output and returns a set of
    /// `Api`s together with some other data.
    pub(crate) fn parse_items(
        mut self,
        items: Vec<Item>,
    ) -> Result<ApiVec<NullPhase>, ConvertError> {
        let items = Self::find_items_in_root(items)?;
        if !self.config.exclude_utilities() {
            generate_utilities(&mut self.apis, self.config);
        }
        self.add_apis_from_config();
        let root_ns = Namespace::new();
        self.parse_mod_items(items, root_ns);
        self.confirm_all_generate_directives_obeyed()?;
        Ok(self.apis)
    }

    /// Some API items are not populated from bindgen output, but instead
    /// directly from items in the config.
    fn add_apis_from_config(&mut self) {
        self.apis
            .extend(self.config.subclasses.iter().map(|sc| Api::Subclass {
                name: SubclassName::new(sc.subclass.clone()),
                superclass: QualifiedName::new_from_cpp_name(&sc.superclass),
            }));
        self.apis
            .extend(self.config.extern_rust_funs.iter().map(|fun| {
                let id = fun.sig.ident.clone();
                Api::RustFn {
                    name: ApiName::new_in_root_namespace(id),
                    path: fun.path.clone(),
                    sig: fun.sig.clone(),
                }
            }));
        self.apis.extend(self.config.rust_types.iter().map(|path| {
            let id = path.get_final_ident();
            Api::RustType {
                name: ApiName::new_in_root_namespace(id.clone()),
                path: path.clone(),
            }
        }));
    }

    fn find_items_in_root(items: Vec<Item>) -> Result<Vec<Item>, ConvertError> {
        for item in items {
            match item {
                Item::Mod(root_mod) => {
                    // With namespaces enabled, bindgen always puts everything
                    // in a mod called 'root'. We don't want to pass that
                    // onto cxx, so jump right into it.
                    assert!(root_mod.ident == "root");
                    if let Some((_, items)) = root_mod.content {
                        return Ok(items);
                    }
                }
                _ => return Err(ConvertError::UnexpectedOuterItem),
            }
        }
        Ok(Vec::new())
    }

    /// Interpret the bindgen-generated .rs for a particular
    /// mod, which corresponds to a C++ namespace.
    fn parse_mod_items(&mut self, items: Vec<Item>, ns: Namespace) {
        // This object maintains some state specific to this namespace, i.e.
        // this particular mod.
        let mut mod_converter = ParseForeignMod::new(ns.clone());
        let mut more_apis = ApiVec::new();
        for item in items {
            report_any_error(&ns, &mut more_apis, || {
                self.parse_item(item, &mut mod_converter, &ns)
            });
        }
        self.apis.append(&mut more_apis);
        mod_converter.finished(&mut self.apis);
    }

    fn parse_item(
        &mut self,
        item: Item,
        mod_converter: &mut ParseForeignMod,
        ns: &Namespace,
    ) -> Result<(), ConvertErrorWithContext> {
        match item {
            Item::ForeignMod(fm) => {
                mod_converter.convert_foreign_mod_items(fm.items);
                Ok(())
            }
            Item::Struct(s) => {
                if s.ident.to_string().ends_with("__bindgen_vtable") {
                    return Ok(());
                }
                let is_forward_declaration = Self::spot_forward_declaration(&s.fields);
                let annotations = BindgenSemanticAttributes::new(&s.attrs);
                // cxx::bridge can't cope with type aliases to generic
                // types at the moment.
                let name = api_name_qualified(ns, s.ident.clone(), &annotations)?;
                let api = if ns.is_empty() && self.config.is_rust_type(&s.ident) {
                    None
                } else if is_forward_declaration {
                    Some(UnanalyzedApi::ForwardDeclaration { name })
                } else {
                    let has_rvalue_reference_fields = s.fields.iter().any(|f| {
                        BindgenSemanticAttributes::new(&f.attrs).has_attr("rvalue_reference")
                    });
                    Some(UnanalyzedApi::Struct {
                        name,
                        details: Box::new(StructDetails {
                            vis: annotations.get_cpp_visibility(),
                            layout: annotations.get_layout(),
                            item: s,
                            has_rvalue_reference_fields,
                        }),
                        analysis: (),
                    })
                };
                if let Some(api) = api {
                    if !self.config.is_on_blocklist(&api.name().to_cpp_name()) {
                        self.apis.push_eliminating_duplicates(api);
                    }
                }
                Ok(())
            }
            Item::Enum(e) => {
                let annotations = BindgenSemanticAttributes::new(&e.attrs);
                let api = UnanalyzedApi::Enum {
                    name: api_name_qualified(ns, e.ident.clone(), &annotations)?,
                    item: e,
                };
                if !self.config.is_on_blocklist(&api.name().to_cpp_name()) {
                    self.apis.push_eliminating_duplicates(api);
                }
                Ok(())
            }
            Item::Impl(imp) => {
                // We *mostly* ignore all impl blocks generated by bindgen.
                // Methods also appear in 'extern "C"' blocks which
                // we will convert instead. At that time we'll also construct
                // synthetic impl blocks.
                // We do however record which methods were spotted, since
                // we have no other way of working out which functions are
                // static methods vs plain functions.
                mod_converter.convert_impl_items(imp);
                Ok(())
            }
            Item::Mod(itm) => {
                if let Some((_, items)) = itm.content {
                    let new_ns = ns.push(itm.ident.to_string());
                    self.parse_mod_items(items, new_ns);
                }
                Ok(())
            }
            Item::Use(use_item) => {
                let mut segs = Vec::new();
                let mut tree = &use_item.tree;
                loop {
                    match tree {
                        UseTree::Path(up) => {
                            segs.push(up.ident.clone());
                            tree = &up.tree;
                        }
                        UseTree::Name(un) if un.ident == "root" => break, // we do not add this to any API since we generate equivalent
                        // use statements in our codegen phase.
                        UseTree::Rename(urn) => {
                            let old_id = &urn.ident;
                            let new_id = &urn.rename;
                            let new_tyname = QualifiedName::new(ns, new_id.clone());
                            assert!(segs.remove(0) == "self", "Path didn't start with self");
                            assert!(
                                segs.remove(0) == "super",
                                "Path didn't start with self::super"
                            );
                            // This is similar to the path encountered within 'tree'
                            // but without the self::super prefix which is unhelpful
                            // in our output mod, because we prefer relative paths
                            // (we're nested in another mod)
                            let old_path: TypePath = parse_quote! {
                                #(#segs)::* :: #old_id
                            };
                            let old_tyname = QualifiedName::from_type_path(&old_path);
                            if new_tyname == old_tyname {
                                return Err(ConvertErrorWithContext(
                                    ConvertError::InfinitelyRecursiveTypedef(new_tyname),
                                    Some(ErrorContext::Item(new_id.clone())),
                                ));
                            }
                            let annotations = BindgenSemanticAttributes::new(&use_item.attrs);
                            self.apis
                                .push_eliminating_duplicates(UnanalyzedApi::Typedef {
                                    name: api_name(ns, new_id.clone(), &annotations),
                                    item: TypedefKind::Use(parse_quote! {
                                        pub use #old_path as #new_id;
                                    }),
                                    old_tyname: Some(old_tyname),
                                    analysis: (),
                                });
                            break;
                        }
                        _ => {
                            return Err(ConvertErrorWithContext(
                                ConvertError::UnexpectedUseStatement(segs.into_iter().last()),
                                None,
                            ))
                        }
                    }
                }
                Ok(())
            }
            Item::Const(const_item) => {
                let annotations = BindgenSemanticAttributes::new(&const_item.attrs);
                self.apis.push_eliminating_duplicates(UnanalyzedApi::Const {
                    name: api_name(ns, const_item.ident.clone(), &annotations),
                    const_item,
                });
                Ok(())
            }
            Item::Type(ity) => {
                let annotations = BindgenSemanticAttributes::new(&ity.attrs);
                // It's known that sometimes bindgen will give us duplicate typedefs with the
                // same name - see test_issue_264.
                self.apis
                    .push_eliminating_duplicates(UnanalyzedApi::Typedef {
                        name: api_name(ns, ity.ident.clone(), &annotations),
                        item: TypedefKind::Type(ity),
                        old_tyname: None,
                        analysis: (),
                    });
                Ok(())
            }
            _ => Err(ConvertErrorWithContext(
                ConvertError::UnexpectedItemInMod,
                None,
            )),
        }
    }

    fn spot_forward_declaration(s: &Fields) -> bool {
        s.iter()
            .filter_map(|f| f.ident.as_ref())
            .any(|id| id == "_unused")
    }

    fn confirm_all_generate_directives_obeyed(&self) -> Result<(), ConvertError> {
        let api_names: HashSet<_> = self
            .apis
            .iter()
            .map(|api| api.name().to_cpp_name())
            .collect();
        for generate_directive in self.config.must_generate_list() {
            if !api_names.contains(&generate_directive) {
                return Err(ConvertError::DidNotGenerateAnything(generate_directive));
            }
        }
        Ok(())
    }
}
