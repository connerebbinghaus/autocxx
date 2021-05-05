// Copyright 2020 Google LLC
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//    https://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use syn::Attribute;

pub(crate) mod abstract_types;
pub(crate) mod ctypes;
pub(crate) mod fun;
pub(crate) mod gc;
pub(crate) mod pod; // hey, that rhymes
pub(crate) mod remove_ignored;
pub(crate) mod tdef;
mod type_converter;

// Remove `bindgen_` attributes. They don't have a corresponding macro defined anywhere,
// so they will cause compilation errors if we leave them in.
fn remove_bindgen_attrs(attrs: &mut Vec<Attribute>) {
    fn is_bindgen_attr(attr: &Attribute) -> bool {
        let segments = &attr.path.segments;
        segments.len() == 1
            && segments
                .first()
                .unwrap()
                .ident
                .to_string()
                .starts_with("bindgen_")
    }

    attrs.retain(|a| !is_bindgen_attr(a))
}
