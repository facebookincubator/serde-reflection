// Copyright (c) Facebook, Inc. and its affiliates
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::analyzer;
use serde_reflection::{ContainerFormat, Format, Named, Registry, VariantFormat};
use std::collections::{BTreeMap, HashSet};
use std::io::{Result, Write};

// TODO:
// * optional namespace
// * optionally use `using` for newtype/tuple structs and variants, as well as enums

pub fn output(
    out: &mut dyn Write,
    registry: &Registry,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    output_preambule(out)?;

    let dependencies = analyzer::get_dependency_map(registry)?;
    let entries = analyzer::best_effort_topological_sort(&dependencies);
    let mut known_names = HashSet::new();
    let mut known_sizes = HashSet::new();
    for name in entries {
        for dependency in &dependencies[name] {
            if !known_names.contains(dependency) {
                output_container_forward_definition(out, *dependency)?;
                known_names.insert(*dependency);
            }
        }
        let format = &registry[name];
        output_container(out, name, format, &known_sizes)?;
        known_sizes.insert(name);
        known_names.insert(name);
    }

    writeln!(out)?;
    for (name, format) in registry {
        output_container_traits(out, name, format)?;
    }
    Ok(())
}

fn output_preambule(out: &mut dyn std::io::Write) -> Result<()> {
    writeln!(out, "#include \"serde.hpp\"\n")
}

/// If known_sizes is present, we must try to return a type with a known size as well.
fn quote_type(format: &Format, known_sizes: Option<&HashSet<&str>>, namespace: &str) -> String {
    use Format::*;
    match format {
        TypeName(x) => {
            if let Some(set) = known_sizes {
                if !set.contains(x.as_str()) {
                    return format!("std::unique_ptr<{}{}>", namespace, x);
                }
            }
            format!("{}{}", namespace, x)
        }
        Unit => "std::monostate".into(),
        Bool => "bool".into(),
        I8 => "int8_t".into(),
        I16 => "int16_t".into(),
        I32 => "int32_t".into(),
        I64 => "int64_t".into(),
        I128 => "int128_t".into(),
        U8 => "uint8_t".into(),
        U16 => "uint16_t".into(),
        U32 => "uint32_t".into(),
        U64 => "uint64_t".into(),
        U128 => "uint128_t".into(),
        F32 => "float".into(),
        F64 => "double".into(),
        Char => "char".into(),
        Str => "std::string".into(),
        Bytes => "std::vector<uint8_t>".into(),

        Option(format) => format!(
            "std::optional<{}>",
            quote_type(format, known_sizes, namespace)
        ),
        Seq(format) => format!("std::vector<{}>", quote_type(format, None, namespace)),
        Map { key, value } => format!(
            "std::map<{}, {}>",
            quote_type(key, None, namespace),
            quote_type(value, None, namespace)
        ),
        Tuple(formats) => format!(
            "std::tuple<{}>",
            quote_types(formats, known_sizes, namespace)
        ),
        TupleArray { content, size } => format!(
            "std::array<{}, {}>",
            quote_type(content, known_sizes, namespace),
            *size
        ),

        Variable(_) => panic!("unexpected value"),
    }
}

fn quote_types(formats: &[Format], known_sizes: Option<&HashSet<&str>>, namespace: &str) -> String {
    formats
        .iter()
        .map(|x| quote_type(x, known_sizes, namespace))
        .collect::<Vec<_>>()
        .join(", ")
}

fn output_fields(
    out: &mut dyn std::io::Write,
    indentation: usize,
    fields: &[Named<Format>],
    known_sizes: &HashSet<&str>,
    namespace: &str,
) -> Result<()> {
    let tab = " ".repeat(indentation);
    for field in fields {
        writeln!(
            out,
            "{}{} {};",
            tab,
            quote_type(&field.value, Some(known_sizes), namespace),
            field.name
        )?;
    }
    Ok(())
}

fn output_variant(
    out: &mut dyn std::io::Write,
    name: &str,
    variant: &VariantFormat,
    known_sizes: &HashSet<&str>,
) -> Result<()> {
    use VariantFormat::*;
    match variant {
        Unit => writeln!(out, "    struct {} {{}};", name),
        NewType(format) => writeln!(
            out,
            "    struct {} {{\n        {} value;\n    }};",
            name,
            quote_type(format, Some(known_sizes), "::")
        ),
        Tuple(formats) => writeln!(
            out,
            "    struct {} {{\n        std::tuple<{}> value;\n    }};",
            name,
            quote_types(formats, Some(known_sizes), "::")
        ),
        Struct(fields) => {
            writeln!(out, "    struct {} {{", name)?;
            output_fields(out, 8, fields, known_sizes, "::")?;
            writeln!(out, "    }};")
        }
        Variable(_) => panic!("incorrect value"),
    }
}

fn output_variants(
    out: &mut dyn std::io::Write,
    variants: &BTreeMap<u32, Named<VariantFormat>>,
    known_sizes: &HashSet<&str>,
) -> Result<()> {
    for (expected_index, (index, variant)) in variants.iter().enumerate() {
        assert_eq!(*index, expected_index as u32);
        output_variant(out, &variant.name, &variant.value, known_sizes)?;
    }
    Ok(())
}

fn output_container_forward_definition(out: &mut dyn std::io::Write, name: &str) -> Result<()> {
    writeln!(out, "struct {};\n", name)
}

fn output_container(
    out: &mut dyn std::io::Write,
    name: &str,
    format: &ContainerFormat,
    known_sizes: &HashSet<&str>,
) -> Result<()> {
    use ContainerFormat::*;
    match format {
        UnitStruct => writeln!(out, "struct {} {{}};\n", name),
        NewTypeStruct(format) => writeln!(
            out,
            "struct {} {{\n    {} value;\n}};\n",
            name,
            quote_type(format, Some(known_sizes), ""),
        ),
        TupleStruct(formats) => writeln!(
            out,
            "struct {} {{\n    std::tuple<{}> value;\n}};\n",
            name,
            quote_types(formats, Some(known_sizes), ""),
        ),
        Struct(fields) => {
            writeln!(out, "struct {} {{", name)?;
            output_fields(out, 4, fields, known_sizes, "")?;
            writeln!(out, "}};\n")
        }
        Enum(variants) => {
            writeln!(out, "struct {} {{", name)?;
            output_variants(out, variants, known_sizes)?;
            writeln!(
                out,
                "    std::variant<{}> value;\n}};\n",
                variants
                    .iter()
                    .map(|(_, v)| v.name.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
            )
        }
    }
}

fn output_struct_serializable(
    out: &mut dyn std::io::Write,
    name: &str,
    fields: &[&str],
) -> Result<()> {
    writeln!(
        out,
        r#"
template <>
template <typename Serializer>
void Serializable<{}>::serialize(const {} &obj, Serializer &serializer) {{"#,
        name, name,
    )?;
    for field in fields {
        writeln!(
            out,
            "    Serializable<decltype(obj.{})>::serialize(obj.{}, serializer);",
            field, field,
        )?;
    }
    writeln!(out, "}}")
}

fn output_struct_deserializable(
    out: &mut dyn std::io::Write,
    name: &str,
    fields: &[&str],
) -> Result<()> {
    writeln!(
        out,
        r#"
template <>
template <typename Deserializer>
{} Deserializable<{}>::deserialize(Deserializer &deserializer) {{
    {} obj;"#,
        name, name, name,
    )?;
    for field in fields {
        writeln!(
            out,
            "    obj.{} = Deserializable<decltype(obj.{})>::deserialize(deserializer);",
            field, field,
        )?;
    }
    writeln!(out, "    return obj;\n}}")
}

fn output_struct_traits(out: &mut dyn std::io::Write, name: &str, fields: &[&str]) -> Result<()> {
    output_struct_serializable(out, name, fields)?;
    output_struct_deserializable(out, name, fields)
}

fn get_variant_fields(format: &VariantFormat) -> Vec<&str> {
    use VariantFormat::*;
    match format {
        Unit => Vec::new(),
        NewType(_format) => vec!["value"],
        Tuple(_formats) => vec!["value"],
        Struct(fields) => fields
            .iter()
            .map(|field| field.name.as_str())
            .collect::<Vec<_>>(),
        Variable(_) => panic!("incorrect value"),
    }
}

fn output_container_traits(
    out: &mut dyn std::io::Write,
    name: &str,
    format: &ContainerFormat,
) -> Result<()> {
    use ContainerFormat::*;
    match format {
        UnitStruct => output_struct_traits(out, name, &[]),
        NewTypeStruct(_format) => output_struct_traits(out, name, &["value"]),
        TupleStruct(_formats) => output_struct_traits(out, name, &["value"]),
        Struct(fields) => output_struct_traits(
            out,
            name,
            &fields
                .iter()
                .map(|field| field.name.as_str())
                .collect::<Vec<_>>(),
        ),
        Enum(variants) => {
            output_struct_traits(out, name, &["value"])?;
            for variant in variants.values() {
                output_struct_traits(
                    out,
                    &format!("{}::{}", name, variant.name),
                    &get_variant_fields(&variant.value),
                )?;
            }
            Ok(())
        }
    }
}
