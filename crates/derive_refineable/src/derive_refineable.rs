use proc_macro::TokenStream;
use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use std::{env, fs, path::PathBuf};
use syn::{
    DeriveInput, Field, FieldsNamed, Generics, Ident, PredicateType, TraitBound, Type,
    TypeParamBound, Visibility, WhereClause, WherePredicate, parse_macro_input, parse_quote,
};

#[proc_macro_derive(Refineable, attributes(refineable))]
pub fn derive_refineable(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_refineable(input, refineable_crate_path()).into()
}

fn expand_refineable(input: DeriveInput, refineable: TokenStream2) -> TokenStream2 {
    let DeriveInput {
        ident,
        data,
        generics,
        attrs,
        ..
    } = input;
    let options = match RefinementOptions::parse(&attrs) {
        Ok(options) => options,
        Err(error) => return error.into_compile_error(),
    };
    let fields = match data {
        syn::Data::Struct(syn::DataStruct {
            fields: syn::Fields::Named(FieldsNamed { named, .. }),
            ..
        }) => named
            .into_iter()
            .map(|field| FieldDescription::new(field, &refineable))
            .collect::<Vec<_>>(),
        _ => panic!("This derive macro only supports structs with named fields"),
    };
    let expansion = RefinementExpansion {
        refinement_ident: format_ident!("{}Refinement", ident),
        where_clause: refinement_where_clause(generics.where_clause.as_ref(), &fields),
        ident,
        generics,
        fields,
        options,
        refineable,
    };
    let structure = generate_refinement_struct(&expansion);
    let original = generate_original_impl(&expansion);
    let refinement = generate_refinement_impl(&expansion);
    let empty = generate_empty_impl(&expansion);
    let conversion = generate_conversion_impl(&expansion);
    let default = generate_default_impl(&expansion);
    let inherent = generate_inherent_impl(&expansion);
    let debug = generate_debug_impl(&expansion);
    quote! { #structure #original #refinement #empty #conversion #default #inherent #debug }
}

#[derive(Clone, Copy)]
enum FieldKind {
    Nested,
    Optional,
    Plain,
}

struct FieldDescription {
    ident: Option<Ident>,
    vis: Visibility,
    kind: FieldKind,
    wrapped_type: Type,
}

impl FieldDescription {
    fn new(field: Field, refineable: &TokenStream2) -> Self {
        let kind = if is_refineable_field(&field) {
            FieldKind::Nested
        } else if is_optional_field(&field) {
            FieldKind::Optional
        } else {
            FieldKind::Plain
        };
        let ty = &field.ty;
        let wrapped_type = match kind {
            FieldKind::Nested => parse_quote!(<#ty as #refineable::Refineable>::Refinement),
            FieldKind::Optional => ty.clone(),
            FieldKind::Plain => parse_quote!(::core::option::Option<#ty>),
        };
        Self {
            ident: field.ident,
            vis: field.vis,
            kind,
            wrapped_type,
        }
    }
}

struct RefinementOptions {
    impl_debug_on_refinement: bool,
    derives_serialize: bool,
    refinement_traits_to_derive: Vec<syn::Path>,
}

impl RefinementOptions {
    fn parse(attrs: &[syn::Attribute]) -> syn::Result<Self> {
        let refineable_attr = attrs.iter().find(|attr| attr.path().is_ident("refineable"));

        let mut impl_debug_on_refinement = false;
        let mut derives_serialize = false;
        let mut refinement_traits_to_derive = vec![];

        if let Some(refineable_attr) = refineable_attr {
            // Propagate syntax errors instead of expanding with silently-dropped
            // options, so malformed attributes fail at the attribute itself.
            refineable_attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("Debug") {
                    impl_debug_on_refinement = true;
                } else {
                    if meta.path.is_ident("Serialize") {
                        derives_serialize = true;
                    }
                    refinement_traits_to_derive.push(meta.path);
                }
                Ok(())
            })?;
        }

        Ok(Self {
            impl_debug_on_refinement,
            derives_serialize,
            refinement_traits_to_derive,
        })
    }
}

struct RefinementExpansion {
    ident: Ident,
    refinement_ident: Ident,
    generics: Generics,
    fields: Vec<FieldDescription>,
    options: RefinementOptions,
    refineable: TokenStream2,
    where_clause: WhereClause,
}

fn refinement_where_clause(
    where_clause: Option<&WhereClause>,
    fields: &[FieldDescription],
) -> WhereClause {
    // Create trait bound that each wrapped type must implement Clone
    let type_param_bounds: Vec<_> = fields
        .iter()
        .map(|field| {
            let ty = &field.wrapped_type;
            WherePredicate::Type(PredicateType {
                attrs: Vec::new(),
                lifetimes: None,
                bounded_ty: ty.clone(),
                colon_token: Default::default(),
                bounds: {
                    let mut punctuated = syn::punctuated::Punctuated::new();
                    punctuated.push_value(TypeParamBound::Trait(TraitBound {
                        paren_token: None,
                        modifiers: syn::TraitBoundModifiers::default(),
                        maybe: None,
                        lifetimes: None,
                        path: parse_quote!(::core::clone::Clone),
                    }));

                    punctuated
                },
            })
        })
        .collect();

    // Append to where_clause or create a new one if it doesn't exist
    match where_clause.cloned() {
        Some(mut where_clause) => {
            where_clause.predicates.extend(type_param_bounds);
            where_clause
        }
        None => WhereClause {
            where_token: Default::default(),
            predicates: type_param_bounds.into_iter().collect(),
        },
    }
}

fn generate_refinement_struct(expansion: &RefinementExpansion) -> TokenStream2 {
    let RefinementExpansion {
        refinement_ident,
        fields,
        options,
        refineable,
        generics,
        ..
    } = expansion;
    let (impl_generics, _, struct_where_clause) = generics.split_for_impl();
    let field_names: Vec<_> = fields.iter().map(|f| f.ident.as_ref().unwrap()).collect();
    let field_visibilities: Vec<_> = fields.iter().map(|f| &f.vis).collect();
    let wrapped_types: Vec<_> = fields.iter().map(|f| f.wrapped_type.clone()).collect();
    let is_empty_path = syn::LitStr::new(
        &format!(
            "{}::IsEmpty::is_empty",
            refineable.to_string().replace(' ', "")
        ),
        proc_macro2::Span::call_site(),
    );

    let field_attributes: Vec<TokenStream2> = fields
        .iter()
        .map(|f| {
            if options.derives_serialize {
                if matches!(f.kind, FieldKind::Nested) {
                    quote! { #[serde(default, skip_serializing_if = #is_empty_path)] }
                } else {
                    quote! { #[serde(skip_serializing_if = "::std::option::Option::is_none")] }
                }
            } else {
                quote! {}
            }
        })
        .collect();

    let mut derive_stream = quote! {};
    for trait_to_derive in &options.refinement_traits_to_derive {
        derive_stream.extend(quote! { #[derive(#trait_to_derive)] })
    }
    quote! {
        /// A refinable version of [`#ident`], see that documentation for details.
        #[derive(Clone)]
        #derive_stream
        pub struct #refinement_ident #impl_generics #struct_where_clause {
            #(
                #[allow(missing_docs)]
                #field_attributes
                #field_visibilities #field_names: #wrapped_types
            ),*
        }
    }
}

fn generate_original_impl(expansion: &RefinementExpansion) -> TokenStream2 {
    let RefinementExpansion {
        ident,
        refinement_ident,
        fields,
        refineable,
        where_clause,
        generics,
        ..
    } = expansion;
    let (impl_generics, ty_generics, _) = generics.split_for_impl();
    let refineable_refine_assignments: Vec<TokenStream2> = fields
        .iter()
        .map(|field| {
            let name = &field.ident;
            let is_refineable = matches!(field.kind, FieldKind::Nested);
            let is_optional = matches!(field.kind, FieldKind::Optional);

            if is_refineable {
                quote! {
                    #refineable::Refineable::refine(&mut self.#name, &refinement.#name);
                }
            } else if is_optional {
                quote! {
                    if let Some(value) = &refinement.#name {
                        self.#name = Some(value.clone());
                    }
                }
            } else {
                quote! {
                    if let Some(value) = &refinement.#name {
                        self.#name = value.clone();
                    }
                }
            }
        })
        .collect();

    let refineable_refined_assignments: Vec<TokenStream2> = fields
        .iter()
        .map(|field| {
            let name = &field.ident;
            let is_refineable = matches!(field.kind, FieldKind::Nested);
            let is_optional = matches!(field.kind, FieldKind::Optional);

            if is_refineable {
                quote! {
                    self.#name = #refineable::Refineable::refined(self.#name, refinement.#name);
                }
            } else if is_optional {
                quote! {
                    if let Some(value) = refinement.#name {
                        self.#name = Some(value);
                    }
                }
            } else {
                quote! {
                    if let Some(value) = refinement.#name {
                        self.#name = value;
                    }
                }
            }
        })
        .collect();

    let refineable_is_superset_conditions: Vec<TokenStream2> = fields
        .iter()
        .map(|field| {
            let name = &field.ident;
            let is_refineable = matches!(field.kind, FieldKind::Nested);
            let is_optional = matches!(field.kind, FieldKind::Optional);

            if is_refineable {
                quote! {
                    if !#refineable::Refineable::is_superset_of(&self.#name, &refinement.#name) {
                        return false;
                    }
                }
            } else if is_optional {
                quote! {
                    if refinement.#name.is_some() && &self.#name != &refinement.#name {
                        return false;
                    }
                }
            } else {
                quote! {
                    if let Some(refinement_value) = &refinement.#name {
                        if &self.#name != refinement_value {
                            return false;
                        }
                    }
                }
            }
        })
        .collect();

    let refineable_subtract_assignments: Vec<TokenStream2> = fields
        .iter()
        .map(|field| {
            let name = &field.ident;
            let is_refineable = matches!(field.kind, FieldKind::Nested);
            let is_optional = matches!(field.kind, FieldKind::Optional);

            if is_refineable {
                quote! {
                    #name: #refineable::Refineable::subtract(&self.#name, &refinement.#name),
                }
            } else if is_optional {
                quote! {
                    #name: if &self.#name == &refinement.#name {
                        None
                    } else {
                        self.#name.clone()
                    },
                }
            } else {
                quote! {
                    #name: if let Some(refinement_value) = &refinement.#name {
                        if &self.#name == refinement_value {
                            None
                        } else {
                            Some(self.#name.clone())
                        }
                    } else {
                        Some(self.#name.clone())
                    },
                }
            }
        })
        .collect();

    quote! {
        impl #impl_generics #refineable::Refineable for #ident #ty_generics
            #where_clause
        {
            type Refinement = #refinement_ident #ty_generics;

            fn refine(&mut self, refinement: &Self::Refinement) {
                #( #refineable_refine_assignments )*
            }

            fn refined(mut self, refinement: Self::Refinement) -> Self {
                #( #refineable_refined_assignments )*
                self
            }

            fn is_superset_of(&self, refinement: &Self::Refinement) -> bool
            {
                #( #refineable_is_superset_conditions )*
                true
            }

            fn subtract(&self, refinement: &Self::Refinement) -> Self::Refinement
            {
                #refinement_ident {
                    #( #refineable_subtract_assignments )*
                }
            }
        }
    }
}

fn generate_refinement_impl(expansion: &RefinementExpansion) -> TokenStream2 {
    let RefinementExpansion {
        refinement_ident,
        fields,
        refineable,
        where_clause,
        generics,
        ..
    } = expansion;
    let (impl_generics, ty_generics, _) = generics.split_for_impl();
    let refinement_refine_assignments: Vec<TokenStream2> = fields
        .iter()
        .map(|field| {
            let name = &field.ident;
            let is_refineable = matches!(field.kind, FieldKind::Nested);

            if is_refineable {
                quote! {
                    #refineable::Refineable::refine(&mut self.#name, &refinement.#name);
                }
            } else {
                quote! {
                    if let Some(value) = &refinement.#name {
                        self.#name = Some(value.clone());
                    }
                }
            }
        })
        .collect();

    let refinement_refined_assignments: Vec<TokenStream2> = fields
        .iter()
        .map(|field| {
            let name = &field.ident;
            let is_refineable = matches!(field.kind, FieldKind::Nested);

            if is_refineable {
                quote! {
                    self.#name = #refineable::Refineable::refined(self.#name, refinement.#name);
                }
            } else {
                quote! {
                    if let Some(value) = refinement.#name {
                        self.#name = Some(value);
                    }
                }
            }
        })
        .collect();

    let refinement_is_superset_conditions: Vec<TokenStream2> = fields
        .iter()
        .map(|field| {
            let name = &field.ident;
            let is_refineable = matches!(field.kind, FieldKind::Nested);

            if is_refineable {
                quote! {
                    if !#refineable::Refineable::is_superset_of(&self.#name, &refinement.#name) {
                        return false;
                    }
                }
            } else {
                quote! {
                    if refinement.#name.is_some() && &self.#name != &refinement.#name {
                        return false;
                    }
                }
            }
        })
        .collect();

    let refinement_subtract_assignments: Vec<TokenStream2> = fields
        .iter()
        .map(|field| {
            let name = &field.ident;
            let is_refineable = matches!(field.kind, FieldKind::Nested);

            if is_refineable {
                quote! {
                    #name: #refineable::Refineable::subtract(&self.#name, &refinement.#name),
                }
            } else {
                quote! {
                    #name: if &self.#name == &refinement.#name {
                        None
                    } else {
                        self.#name.clone()
                    },
                }
            }
        })
        .collect();

    quote! {
        impl #impl_generics #refineable::Refineable for #refinement_ident #ty_generics
            #where_clause
        {
            type Refinement = #refinement_ident #ty_generics;

            fn refine(&mut self, refinement: &Self::Refinement) {
                #( #refinement_refine_assignments )*
            }

            fn refined(mut self, refinement: Self::Refinement) -> Self {
                #( #refinement_refined_assignments )*
                self
            }

            fn is_superset_of(&self, refinement: &Self::Refinement) -> bool
            {
                #( #refinement_is_superset_conditions )*
                true
            }

            fn subtract(&self, refinement: &Self::Refinement) -> Self::Refinement
            {
                #refinement_ident {
                    #( #refinement_subtract_assignments )*
                }
            }
        }
    }
}

fn generate_empty_impl(expansion: &RefinementExpansion) -> TokenStream2 {
    let RefinementExpansion {
        refinement_ident,
        fields,
        refineable,
        where_clause,
        generics,
        ..
    } = expansion;
    let (impl_generics, ty_generics, _) = generics.split_for_impl();
    let refinement_is_empty_conditions: Vec<TokenStream2> = fields
        .iter()
        .enumerate()
        .map(|(i, field)| {
            let name = &field.ident;

            let condition = if matches!(field.kind, FieldKind::Nested) {
                quote! { #refineable::IsEmpty::is_empty(&self.#name) }
            } else {
                quote! { self.#name.is_none() }
            };

            if i < fields.len() - 1 {
                quote! { #condition && }
            } else {
                condition
            }
        })
        .collect();

    quote! {
        impl #impl_generics #refineable::IsEmpty for #refinement_ident #ty_generics
            #where_clause
        {
            fn is_empty(&self) -> bool {
                #( #refinement_is_empty_conditions )*
            }
        }
    }
}

fn generate_conversion_impl(expansion: &RefinementExpansion) -> TokenStream2 {
    let RefinementExpansion {
        ident,
        refinement_ident,
        fields,
        where_clause,
        generics,
        ..
    } = expansion;
    let (impl_generics, ty_generics, _) = generics.split_for_impl();
    let from_refinement_assignments: Vec<TokenStream2> = fields
        .iter()
        .map(|field| {
            let name = &field.ident;
            let is_refineable = matches!(field.kind, FieldKind::Nested);
            let is_optional = matches!(field.kind, FieldKind::Optional);

            if is_refineable {
                quote! {
                    #name: value.#name.into(),
                }
            } else if is_optional {
                quote! {
                    #name: value.#name.map(|v| v.into()),
                }
            } else {
                quote! {
                    #name: value.#name.map(|v| v.into()).unwrap_or_default(),
                }
            }
        })
        .collect();

    quote! {
        impl #impl_generics ::core::convert::From<#refinement_ident #ty_generics> for #ident #ty_generics
            #where_clause
        {
            fn from(value: #refinement_ident #ty_generics) -> Self {
                Self {
                    #( #from_refinement_assignments )*
                }
            }
        }
    }
}

fn generate_default_impl(expansion: &RefinementExpansion) -> TokenStream2 {
    let RefinementExpansion {
        refinement_ident,
        fields,
        where_clause,
        generics,
        ..
    } = expansion;
    let (impl_generics, ty_generics, _) = generics.split_for_impl();
    let field_names: Vec<_> = fields.iter().map(|f| f.ident.as_ref().unwrap()).collect();
    quote! {
        impl #impl_generics ::core::default::Default for #refinement_ident #ty_generics
            #where_clause
        {
            fn default() -> Self {
                #refinement_ident {
                    #( #field_names: ::core::default::Default::default() ),*
                }
            }
        }
    }
}

fn generate_inherent_impl(expansion: &RefinementExpansion) -> TokenStream2 {
    let RefinementExpansion {
        refinement_ident,
        fields,
        refineable,
        where_clause,
        generics,
        ..
    } = expansion;
    let (impl_generics, ty_generics, _) = generics.split_for_impl();
    let is_some_conditions = fields.iter().map(|field| {
        let name = &field.ident;
        if matches!(field.kind, FieldKind::Nested) {
            quote! { !#refineable::IsEmpty::is_empty(&self.#name) }
        } else {
            quote! { self.#name.is_some() }
        }
    });

    quote! {
        impl #impl_generics #refinement_ident #ty_generics
            #where_clause
        {
            /// Returns `true` if any field is `Some`
            pub fn is_some(&self) -> bool {
                #(
                    if #is_some_conditions {
                        return true;
                    }
                )*
                false
            }
        }
    }
}

fn generate_debug_impl(expansion: &RefinementExpansion) -> TokenStream2 {
    let RefinementExpansion {
        refinement_ident,
        generics,
        fields,
        options,
        refineable,
        where_clause,
        ..
    } = expansion;
    let (impl_generics, ty_generics, _) = generics.split_for_impl();
    let debug_impl = if options.impl_debug_on_refinement {
        let refinement_field_debugs: Vec<TokenStream2> = fields
            .iter()
            .map(|field| {
                let name = &field.ident;
                let is_some = if matches!(field.kind, FieldKind::Nested) {
                    quote! { !#refineable::IsEmpty::is_empty(&self.#name) }
                } else {
                    quote! { self.#name.is_some() }
                };
                quote! {
                    if #is_some {
                        debug_struct.field(stringify!(#name), &self.#name);
                    } else {
                        all_some = false;
                    }
                }
            })
            .collect();

        quote! {
            impl #impl_generics std::fmt::Debug for #refinement_ident #ty_generics
                #where_clause
            {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    let mut debug_struct = f.debug_struct(stringify!(#refinement_ident));
                    let mut all_some = true;
                    #( #refinement_field_debugs )*
                    if all_some {
                        debug_struct.finish()
                    } else {
                        debug_struct.finish_non_exhaustive()
                    }
                }
            }
        }
    } else {
        quote! {}
    };

    debug_impl
}

fn is_refineable_field(f: &Field) -> bool {
    f.attrs
        .iter()
        .any(|attr| attr.path().is_ident("refineable"))
}

fn is_optional_field(f: &Field) -> bool {
    if let Type::Path(typepath) = &f.ty
        && typepath.qself.is_none()
    {
        let segments = typepath
            .path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>();
        return matches!(segments.as_slice(), [option] if option == "Option")
            || matches!(segments.as_slice(), [root, module, option] if (root == "std" || root == "core") && module == "option" && option == "Option");
    }
    false
}

fn refineable_crate_path() -> TokenStream2 {
    if let Ok(found) = crate_name("fc-gpui-refineable") {
        return dependency_path(found, "fc_gpui_refineable", "fc_gpui_refineable");
    }

    // The facade and core re-export this derive. Honor the same explicit crate
    // selection as GPUI's other derives when both packages or an underscore alias exist.
    let configured_path = env::var_os("CARGO_MANIFEST_DIR")
        .and_then(|directory| fs::read_to_string(PathBuf::from(directory).join("Cargo.toml")).ok())
        .and_then(|manifest| manifest.parse::<toml_edit::DocumentMut>().ok())
        .and_then(|manifest| {
            manifest
                .get("package")?
                .get("metadata")?
                .get("gpui-macros")?
                .get("crate")?
                .as_str()
                .map(str::to_owned)
        });
    if let Some(path) = configured_path {
        let path = syn::parse_str::<syn::Path>(&path)
            .expect("package.metadata.gpui-macros.crate must be a Rust crate path");
        return quote!(::#path);
    }
    for (package, package_identifier, library_identifier) in [
        ("fc-gpui", "fc_gpui", "gpui"),
        ("fc-gpui-core", "fc_gpui_core", "gpui_core"),
    ] {
        if let Ok(found) = crate_name(package) {
            return dependency_path(found, package_identifier, library_identifier);
        }
    }
    quote!(::refineable)
}

fn dependency_path(
    found: FoundCrate,
    package_identifier: &str,
    library_identifier: &str,
) -> TokenStream2 {
    match found {
        FoundCrate::Itself => {
            if package_identifier == "fc_gpui_refineable" {
                quote!(::fc_gpui_refineable)
            } else {
                quote!(::gpui)
            }
        }
        FoundCrate::Name(name) => {
            let name = if name == package_identifier {
                library_identifier
            } else {
                &name
            };
            let ident = syn::Ident::new(&name.replace('-', "_"), proc_macro2::Span::call_site());
            quote!(::#ident)
        }
    }
}
