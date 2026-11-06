mod nested {
    use super::derive_api;

    #[derive(Clone, Default, PartialEq, derive_api::Refineable)]
    #[refineable(Debug)]
    pub struct Leaf {
        pub value: u32,
    }
}

type LeafAlias = nested::Leaf;

trait NestedField {
    type Value;
}

struct FieldOwner;

impl NestedField for FieldOwner {
    type Value = nested::Leaf;
}

#[derive(Clone, Default, PartialEq, derive_api::Refineable)]
struct AssociatedStyle {
    #[refineable]
    nested: <FieldOwner as NestedField>::Value,
}

#[derive(Clone, Default, PartialEq, derive_api::Refineable)]
#[refineable(Debug)]
struct NestedStyle {
    #[refineable]
    qualified: nested::Leaf,
    #[refineable]
    aliased: LeafAlias,
    optional: std::option::Option<u32>,
}

#[derive(Clone, derive_api::Refineable)]
struct GenericStyle<T>
where
    T: refinement_api::Refineable,
    T::Refinement: Into<T>,
{
    #[refineable]
    nested: T,
}

#[derive(Clone, Default, PartialEq)]
struct ManualStyle(u32);

#[derive(Clone, Debug, Default)]
struct CustomPatch(Option<u32>);

impl refinement_api::IsEmpty for CustomPatch {
    fn is_empty(&self) -> bool {
        self.0.is_none()
    }
}

impl refinement_api::Refineable for CustomPatch {
    type Refinement = Self;
    fn refine(&mut self, patch: &Self) {
        if patch.0.is_some() {
            self.0 = patch.0;
        }
    }
    fn refined(mut self, patch: Self) -> Self {
        refinement_api::Refineable::refine(&mut self, &patch);
        self
    }
    fn is_superset_of(&self, patch: &Self) -> bool {
        patch.0.is_none() || self.0 == patch.0
    }
    fn subtract(&self, patch: &Self) -> Self {
        if self.0 == patch.0 {
            Self(None)
        } else {
            self.clone()
        }
    }
}

impl refinement_api::Refineable for ManualStyle {
    type Refinement = CustomPatch;
    fn refine(&mut self, patch: &CustomPatch) {
        if let Some(value) = patch.0 {
            self.0 = value;
        }
    }
    fn refined(mut self, patch: CustomPatch) -> Self {
        refinement_api::Refineable::refine(&mut self, &patch);
        self
    }
    fn is_superset_of(&self, patch: &CustomPatch) -> bool {
        patch.0.is_none_or(|value| self.0 == value)
    }
    fn subtract(&self, patch: &CustomPatch) -> CustomPatch {
        CustomPatch((patch.0 != Some(self.0)).then_some(self.0))
    }
}

impl From<CustomPatch> for ManualStyle {
    fn from(patch: CustomPatch) -> Self {
        Self(patch.0.unwrap_or_default())
    }
}

#[derive(Clone, Default, PartialEq, derive_api::Refineable)]
#[refineable(Debug)]
struct CustomStyle {
    #[refineable]
    nested: ManualStyle,
}

#[test]
fn nested_refinements_follow_the_trait_associated_type() {
    let mut style = NestedStyle::default();
    let mut patch = NestedStyleRefinement::default();
    assert!(refinement_api::IsEmpty::is_empty(&patch));
    patch.qualified.value = Some(7);
    patch.aliased.value = Some(9);
    patch.optional = Some(11);
    refinement_api::Refineable::refine(&mut style, &patch);
    assert_eq!(style.qualified.value, 7);
    assert_eq!(style.aliased.value, 9);
    assert_eq!(style.optional, Some(11));
    assert!(refinement_api::Refineable::is_superset_of(&style, &patch));
    assert!(refinement_api::IsEmpty::is_empty(
        &refinement_api::Refineable::subtract(&style, &patch)
    ));
    let _ = format!("{patch:?}");

    let mut generic = GenericStyle {
        nested: nested::Leaf::default(),
    };
    let mut patch = GenericStyleRefinement::<nested::Leaf>::default();
    patch.nested.value = Some(13);
    refinement_api::Refineable::refine(&mut generic, &patch);
    assert_eq!(generic.nested.value, 13);

    let mut associated = AssociatedStyle::default();
    let mut patch = AssociatedStyleRefinement::default();
    patch.nested.value = Some(15);
    refinement_api::Refineable::refine(&mut associated, &patch);
    assert_eq!(associated.nested.value, 15);

    let patch = CustomStyleRefinement {
        nested: CustomPatch(Some(17)),
    };
    assert!(patch.is_some());
    assert!(!refinement_api::IsEmpty::is_empty(&patch));
    let _ = format!("{patch:?}");
    let style = refinement_api::Refineable::refined(CustomStyle::default(), patch);
    assert_eq!(style.nested.0, 17);
}
