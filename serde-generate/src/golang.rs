// Copyright (c) Facebook, Inc. and its affiliates
// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::{
    indent::{IndentConfig, IndentedWriter},
    CodeGeneratorConfig,
};
use heck::CamelCase;
use include_dir::include_dir as include_directory;
use serde_reflection::{ContainerFormat, Format, FormatHolder, Named, Registry, VariantFormat};
use std::{
    collections::{BTreeMap, HashMap},
    io::{Result, Write},
    path::PathBuf,
};

/// Main configuration object for code-generation in Go.
pub struct CodeGenerator<'a> {
    /// Language-independent configuration.
    config: &'a CodeGeneratorConfig,
    /// Mapping from external type names to fully-qualified class names (e.g. "MyClass" -> "com.facebook.my_package.MyClass").
    /// Derived from `config.external_definitions`.
    external_qualified_names: HashMap<String, String>,
}

/// Shared state for the code generation of a Go source file.
struct GoEmitter<'a, T> {
    /// Writer.
    out: IndentedWriter<T>,
    /// Generator.
    generator: &'a CodeGenerator<'a>,
    /// Current namespace (e.g. vec!["com", "facebook", "my_package", "MyClass"])
    current_namespace: Vec<String>,
}

impl<'a> CodeGenerator<'a> {
    /// Create a Go code generator for the given config.
    pub fn new(config: &'a CodeGeneratorConfig) -> Self {
        let mut external_qualified_names = HashMap::new();
        for (namespace, names) in &config.external_definitions {
            for name in names {
                external_qualified_names
                    .insert(name.to_string(), format!("{}.{}", namespace, name));
            }
        }
        Self {
            config,
            external_qualified_names,
        }
    }

    /// Output class definitions for `registry`.
    pub fn output(&self, out: &mut dyn Write, registry: &Registry) -> Result<()> {
        let current_namespace = self
            .config
            .module_name
            .split('.')
            .map(String::from)
            .collect::<Vec<_>>();

        let mut emitter = GoEmitter {
            out: IndentedWriter::new(out, IndentConfig::Space(4)),
            generator: self,
            current_namespace,
        };

        emitter.output_preamble()?;

        for (name, format) in registry {
            emitter.output_container(name, format)?;
        }

        emitter.output_trait_helpers(registry)?;

        Ok(())
    }
}

impl<'a, T> GoEmitter<'a, T>
where
    T: Write,
{
    fn output_preamble(&mut self) -> Result<()> {
        writeln!(self.out, "package {}\n", self.generator.config.module_name)?;
        writeln!(
            self.out,
            r#"
import "fmt"
import "serde"
"#
        )?;
        Ok(())
    }

    /// Compute a reference to the registry type `name`.
    fn quote_qualified_name(&self, name: &str) -> String {
        self.generator
            .external_qualified_names
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }

    fn output_comment(&mut self, name: &str) -> std::io::Result<()> {
        let mut path = self.current_namespace.clone();
        path.push(name.to_string());
        if let Some(doc) = self.generator.config.comments.get(&path) {
            let text = textwrap::indent(doc, "// ").replace("\n\n", "\n//\n");
            writeln!(self.out, "{}", text)?;
        }
        Ok(())
    }

    fn quote_type(&self, format: &Format) -> String {
        use Format::*;
        match format {
            TypeName(x) => self.quote_qualified_name(x),
            Unit => "struct {}".into(),
            Bool => "bool".into(),
            I8 => "int8".into(),
            I16 => "int16".into(),
            I32 => "int32".into(),
            I64 => "int64".into(),
            I128 => "serde.Int128".into(),
            U8 => "uint8".into(),
            U16 => "uint16".into(),
            U32 => "uint32".into(),
            U64 => "uint64".into(),
            U128 => "serde.Uint128".into(),
            F32 => "float32".into(),
            F64 => "float64".into(),
            Char => "rune".into(),
            Str => "string".into(),
            Bytes => "[]byte".into(),

            Option(format) => format!("*{}", self.quote_type(format)),
            Seq(format) => format!("[]{}", self.quote_type(format)),
            Map { key, value } => {
                format!("map[{}]{}", self.quote_type(key), self.quote_type(value))
            }
            Tuple(formats) => format!(
                "struct {{{}}}",
                formats
                    .iter()
                    .enumerate()
                    .map(|(index, format)| format!("Field{} {}", index, self.quote_type(format)))
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
            TupleArray { content, size } => format!("[{}]{}", size, self.quote_type(content)),

            Variable(_) => panic!("unexpected value"),
        }
    }

    fn enter_class(&mut self, name: &str) {
        self.out.indent();
        self.current_namespace.push(name.to_string());
    }

    fn leave_class(&mut self) {
        self.out.unindent();
        self.current_namespace.pop();
    }

    fn output_trait_helpers(&mut self, registry: &Registry) -> Result<()> {
        let mut subtypes = BTreeMap::new();
        for format in registry.values() {
            format
                .visit(&mut |f| {
                    if Self::needs_helper(f) {
                        subtypes.insert(Self::mangle_type(f), f.clone());
                    }
                    Ok(())
                })
                .unwrap();
        }
        for (mangled_name, subtype) in &subtypes {
            self.output_serialization_helper(mangled_name, subtype)?;
            self.output_deserialization_helper(mangled_name, subtype)?;
        }
        Ok(())
    }

    // TODO: share
    fn mangle_type(format: &Format) -> String {
        use Format::*;
        match format {
            TypeName(x) => x.to_string(),
            Unit => "unit".into(),
            Bool => "bool".into(),
            I8 => "i8".into(),
            I16 => "i16".into(),
            I32 => "i32".into(),
            I64 => "i64".into(),
            I128 => "i128".into(),
            U8 => "u8".into(),
            U16 => "u16".into(),
            U32 => "u32".into(),
            U64 => "u64".into(),
            U128 => "u128".into(),
            F32 => "f32".into(),
            F64 => "f64".into(),
            Char => "char".into(),
            Str => "str".into(),
            Bytes => "bytes".into(),

            Option(format) => format!("option_{}", Self::mangle_type(format)),
            Seq(format) => format!("vector_{}", Self::mangle_type(format)),
            Map { key, value } => format!(
                "map_{}_to_{}",
                Self::mangle_type(key),
                Self::mangle_type(value)
            ),
            Tuple(formats) => format!(
                "tuple{}_{}",
                formats.len(),
                formats
                    .iter()
                    .map(Self::mangle_type)
                    .collect::<Vec<_>>()
                    .join("_")
            ),
            TupleArray { content, size } => {
                format!("array{}_{}_array", size, Self::mangle_type(content))
            }
            Variable(_) => panic!("unexpected value"),
        }
    }

    fn needs_helper(format: &Format) -> bool {
        use Format::*;
        match format {
            Option(_) | Seq(_) | Map { .. } | Tuple(_) | TupleArray { .. } => true,
            _ => false,
        }
    }

    fn quote_serialize_value(&self, value: &str, format: &Format) -> String {
        use Format::*;
        let expr = match format {
            TypeName(_) => format!("{}.Serialize(serializer)", value),
            Unit => format!("serializer.SerializeUnit({})", value),
            Bool => format!("serializer.SerializeBool({})", value),
            I8 => format!("serializer.SerializeI8({})", value),
            I16 => format!("serializer.SerializeI16({})", value),
            I32 => format!("serializer.SerializeI32({})", value),
            I64 => format!("serializer.SerializeI64({})", value),
            I128 => format!("serializer.SerializeI128({})", value),
            U8 => format!("serializer.SerializeU8({})", value),
            U16 => format!("serializer.SerializeU16({})", value),
            U32 => format!("serializer.SerializeU32({})", value),
            U64 => format!("serializer.SerializeU64({})", value),
            U128 => format!("serializer.SerializeU128({})", value),
            F32 => format!("serializer.SerializeF32({})", value),
            F64 => format!("serializer.SerializeF64({})", value),
            Char => format!("serializer.SerializeChar({})", value),
            Str => format!("serializer.SerializeStr({})", value),
            Bytes => format!("serializer.SerializeBytes({})", value),
            _ => format!(
                "serialize_{}({}, serializer)",
                Self::mangle_type(format),
                value
            ),
        };
        format!("if err := {}; err != nil {{ return err }}", expr)
    }

    fn quote_deserialize(&self, format: &Format, dest: &str, fail: &str) -> String {
        use Format::*;
        let expr = match format {
            TypeName(name) => format!(
                "Deserialize{}(deserializer)",
                self.quote_qualified_name(name)
            ),
            Unit => "deserializer.DeserializeUnit()".to_string(),
            Bool => "deserializer.DeserializeBool()".to_string(),
            I8 => "deserializer.DeserializeI8()".to_string(),
            I16 => "deserializer.DeserializeI16()".to_string(),
            I32 => "deserializer.DeserializeI32()".to_string(),
            I64 => "deserializer.DeserializeI64()".to_string(),
            I128 => "deserializer.DeserializeI128()".to_string(),
            U8 => "deserializer.DeserializeU8()".to_string(),
            U16 => "deserializer.DeserializeU16()".to_string(),
            U32 => "deserializer.DeserializeU32()".to_string(),
            U64 => "deserializer.DeserializeU64()".to_string(),
            U128 => "deserializer.DeserializeU128()".to_string(),
            F32 => "deserializer.DeserializeF32()".to_string(),
            F64 => "deserializer.DeserializeF64()".to_string(),
            Char => "deserializer.DeserializeChar()".to_string(),
            Str => "deserializer.DeserializeStr()".to_string(),
            Bytes => "deserializer.DeserializeBytes()".to_string(),
            _ => format!("deserialize_{}(deserializer)", Self::mangle_type(format)),
        };
        format!(
            "{{ val, err := {}; if err != nil {{ return {}, err }} else {{ {} = val }} }}",
            expr, fail, dest
        )
    }

    fn output_serialization_helper(&mut self, name: &str, format0: &Format) -> Result<()> {
        use Format::*;

        write!(
            self.out,
            "func serialize_{}(value {}, serializer serde.Serializer) error {{",
            name,
            self.quote_type(format0)
        )?;
        self.out.indent();
        match format0 {
            Option(format) => {
                write!(
                    self.out,
                    r#"
if (value != nil) {{
    if err := serializer.SerializeOptionTag(true); err != nil {{ return err }}
    {}
}} else {{
    if err := serializer.SerializeOptionTag(false); err != nil {{ return err }}
}}
"#,
                    self.quote_serialize_value("(*value)", format)
                )?;
            }

            Seq(format) => {
                write!(
                    self.out,
                    r#"
if err := serializer.SerializeLen(len(value)); err != nil {{ return err }}
for _, item := range(value) {{
    {}
}}
"#,
                    self.quote_serialize_value("item", format)
                )?;
            }

            Map { key, value } => {
                write!(
                    self.out,
                    r#"
if err := serializer.SerializeLen(len(value)); err != nil {{ return err }}
offsets := make([]int, len(value))
count := 0
for k, v := range(value) {{
    offsets[count] = serializer.GetBufferOffset()
    count += 1
    {}
    {}
}}
serializer.SortMapEntries(offsets);
"#,
                    self.quote_serialize_value("k", key),
                    self.quote_serialize_value("v", value)
                )?;
            }

            Tuple(formats) => {
                writeln!(self.out)?;
                for (index, format) in formats.iter().enumerate() {
                    let expr = format!("value.Field{}", index);
                    writeln!(self.out, "{}", self.quote_serialize_value(&expr, format))?;
                }
            }

            TupleArray { content, size: _ } => {
                write!(
                    self.out,
                    r#"
for _, item := range(value) {{
    {}
}}
"#,
                    self.quote_serialize_value("item", content),
                )?;
            }

            _ => panic!("unexpected case"),
        }
        writeln!(self.out, "return nil")?;
        self.out.unindent();
        writeln!(self.out, "}}\n")
    }

    fn output_deserialization_helper(&mut self, name: &str, format0: &Format) -> Result<()> {
        use Format::*;

        write!(
            self.out,
            "func deserialize_{}(deserializer serde.Deserializer) ({}, error) {{",
            name,
            self.quote_type(format0),
        )?;
        self.out.indent();
        match format0 {
            Option(format) => {
                write!(
                    self.out,
                    r#"
var value *{}
tag, err := deserializer.DeserializeOptionTag()
if err != nil {{ return value, err }}
if (tag) {{
    {}
}}
return value, nil
"#,
                    self.quote_type(format),
                    self.quote_deserialize(format, "*value", "nil"),
                )?;
            }

            Seq(format) => {
                write!(
                    self.out,
                    r#"
length, err := deserializer.DeserializeLen()
if err != nil {{ return nil, err }}
obj := make([]{}, length)
for i := range(obj) {{
    {}
}}
return obj, nil
"#,
                    self.quote_type(format),
                    self.quote_deserialize(format, "obj[i]", "nil")
                )?;
            }

            Map { key, value } => {
                write!(
                    self.out,
                    r#"
length, err := deserializer.DeserializeLen()
if err != nil {{ return nil, err }}
obj := make(map[{0}]{1})
previous_slice := serde.Slice {{ 0, 0 }}
for i := 0; i < length; i++ {{
    var slice serde.Slice
    slice.Start = deserializer.GetBufferOffset()
    var key {0}
    {2}
    slice.End = deserializer.GetBufferOffset()
    if (i > 0) {{
        err := deserializer.CheckThatKeySlicesAreIncreasing(previous_slice, slice)
        if err != nil {{ return nil, err }}
    }}
    previous_slice = slice
    {3}
}}
return obj, nil
"#,
                    self.quote_type(key),
                    self.quote_type(value),
                    self.quote_deserialize(key, "key", "nil"),
                    self.quote_deserialize(value, "obj[key]", "nil"),
                )?;
            }

            Tuple(formats) => {
                write!(
                    self.out,
                    r#"
var obj {}
{}
return obj, nil
"#,
                    self.quote_type(format0),
                    formats
                        .iter()
                        .enumerate()
                        .map(|(i, f)| self.quote_deserialize(f, &format!("obj.Field{}", i), "obj"))
                        .collect::<Vec<_>>()
                        .join("\n")
                )?;
            }

            TupleArray { content, size } => {
                write!(
                    self.out,
                    r#"
var obj [{1}]{0}
for i := range(obj) {{
    {2}
}}
return obj, nil
"#,
                    self.quote_type(content),
                    size,
                    self.quote_deserialize(content, "obj[i]", "obj")
                )?;
            }

            _ => panic!("unexpected case"),
        }
        self.out.unindent();
        writeln!(self.out, "}}\n")
    }

    fn output_variant(
        &mut self,
        base: &str,
        index: u32,
        name: &str,
        variant: &VariantFormat,
    ) -> Result<()> {
        use VariantFormat::*;
        let fields = match variant {
            Unit => Vec::new(),
            NewType(format) => vec![Named {
                name: "Value".to_string(),
                value: format.as_ref().clone(),
            }],
            Tuple(formats) => formats
                .iter()
                .enumerate()
                .map(|(i, f)| Named {
                    name: format!("Field{}", i),
                    value: f.clone(),
                })
                .collect(),
            Struct(fields) => fields.clone(),
            Variable(_) => panic!("incorrect value"),
        };
        self.output_struct_or_variant_container(Some(base), Some(index), name, &fields)
    }

    fn output_struct_or_variant_container(
        &mut self,
        variant_base: Option<&str>,
        variant_index: Option<u32>,
        name: &str,
        fields: &[Named<Format>],
    ) -> Result<()> {
        let full_name = match variant_base {
            None => name.to_string(),
            Some(base) => format!("{}__{}", base, name),
        };
        // Struct
        writeln!(self.out)?;
        self.output_comment(name)?;
        writeln!(self.out, "type {} struct {{", full_name)?;
        self.enter_class(name);
        for field in fields {
            self.output_comment(&field.name)?;
            writeln!(self.out, "{} {}", field.name, self.quote_type(&field.value))?;
        }
        self.leave_class();
        writeln!(self.out, "}}")?;

        // Link to base interface.
        if let Some(base) = variant_base {
            writeln!(self.out, "\nfunc ({}) is{}() {{}}", full_name, base)?;
        }

        // Serialize
        if self.generator.config.serialization {
            writeln!(
                self.out,
                "\nfunc (obj *{}) Serialize(serializer serde.Serializer) error {{",
                full_name
            )?;
            self.out.indent();
            if let Some(index) = variant_index {
                writeln!(self.out, "serializer.SerializeVariantIndex({})", index)?;
            }
            for field in fields {
                writeln!(
                    self.out,
                    "{}",
                    self.quote_serialize_value(&format!("(*obj).{}", &field.name), &field.value)
                )?;
            }
            writeln!(self.out, "return nil")?;
            self.out.unindent();
            writeln!(self.out, "}}\n")?;
        }
        // Deserialize (struct) or Load (variant)
        if self.generator.config.serialization {
            writeln!(
                self.out,
                "func {0}{1}(deserializer serde.Deserializer) ({1}, error) {{",
                if variant_base.is_none() {
                    "Deserialize"
                } else {
                    "load_"
                },
                full_name,
            )?;
            self.out.indent();
            writeln!(self.out, "var obj {}", full_name)?;
            for field in fields {
                writeln!(
                    self.out,
                    "{}",
                    self.quote_deserialize(&field.value, &format!("obj.{}", field.name), "obj")
                )?;
            }
            writeln!(self.out, "return obj, nil")?;
            self.out.unindent();
            writeln!(self.out, "}}\n")?;
        }
        Ok(())
    }

    fn output_enum_container(
        &mut self,
        name: &str,
        variants: &BTreeMap<u32, Named<VariantFormat>>,
    ) -> Result<()> {
        writeln!(self.out)?;
        self.output_comment(name)?;
        writeln!(self.out, "type {} interface {{", name)?;
        self.current_namespace.push(name.to_string());
        self.out.indent();
        writeln!(self.out, "is{}()", name)?;
        if self.generator.config.serialization {
            writeln!(self.out, "Serialize(serializer serde.Serializer) error")?;
        }
        self.out.unindent();
        writeln!(self.out, "}}")?;

        if self.generator.config.serialization {
            write!(
                self.out,
                "\nfunc Deserialize{0}(deserializer serde.Deserializer) ({0}, error) {{",
                name
            )?;
            self.out.indent();
            writeln!(
                self.out,
                r#"
index, err := deserializer.DeserializeVariantIndex()
if err != nil {{ return nil, err }}

switch index {{"#,
            )?;
            self.out.indent();
            for (index, variant) in variants {
                writeln!(
                    self.out,
                    r#"case {}: {{
    val, err := load_{}__{}(deserializer)
    if err != nil {{
        return nil, err
    }} else {{
        return &val, nil
    }}
}}"#,
                    index, name, variant.name
                )?;
            }
            writeln!(
                self.out,
                "default: return nil, fmt.Errorf(\"Unknown variant index for {}: %d\", index)",
                name,
            )?;
            self.out.unindent();
            writeln!(self.out, "}}")?;
            self.out.unindent();
            writeln!(self.out, "}}")?;
        }

        for (index, variant) in variants {
            self.output_variant(name, *index, &variant.name, &variant.value)?;
        }
        self.current_namespace.pop();
        Ok(())
    }

    fn output_container(&mut self, name: &str, format: &ContainerFormat) -> Result<()> {
        use ContainerFormat::*;
        let fields = match format {
            UnitStruct => Vec::new(),
            NewTypeStruct(format) => vec![Named {
                name: "Value".to_string(),
                value: format.as_ref().clone(),
            }],
            TupleStruct(formats) => formats
                .iter()
                .enumerate()
                .map(|(i, f)| Named {
                    name: format!("Field{}", i),
                    value: f.clone(),
                })
                .collect(),
            Struct(fields) => fields
                .iter()
                .map(|f| Named {
                    name: f.name.to_camel_case(),
                    value: f.value.clone(),
                })
                .collect(),
            Enum(variants) => {
                self.output_enum_container(name, variants)?;
                return Ok(());
            }
        };
        self.output_struct_or_variant_container(None, None, name, &fields)
    }
}

/// Installer for generated source files in Go.
pub struct Installer {
    install_dir: PathBuf,
}

impl Installer {
    pub fn new(install_dir: PathBuf) -> Self {
        Installer { install_dir }
    }

    fn install_runtime(
        &self,
        source_dir: include_dir::Dir,
        path: &str,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let dir_path = self.install_dir.join(path);
        std::fs::create_dir_all(&dir_path)?;
        for entry in source_dir.files() {
            let mut file = std::fs::File::create(dir_path.join(entry.path()))?;
            file.write_all(entry.contents())?;
        }
        Ok(())
    }
}

impl crate::SourceInstaller for Installer {
    type Error = Box<dyn std::error::Error>;

    fn install_module(
        &self,
        config: &CodeGeneratorConfig,
        registry: &Registry,
    ) -> std::result::Result<(), Self::Error> {
        let dir_path = &self.install_dir;
        std::fs::create_dir_all(dir_path)?;
        let source_path = dir_path.join(format!("{}.go", config.module_name));
        let mut file = std::fs::File::create(source_path)?;

        let generator = CodeGenerator::new(config);
        generator.output(&mut file, registry)?;
        Ok(())
    }

    fn install_serde_runtime(&self) -> std::result::Result<(), Self::Error> {
        self.install_runtime(include_directory!("runtime/golang/src/serde"), "src/serde")
    }

    fn install_bincode_runtime(&self) -> std::result::Result<(), Self::Error> {
        /*
                self.install_runtime(
                    include_directory!("runtime/golang/src/bincode"),
                    "src/bincode",
                )
        */
        Ok(())
    }

    fn install_lcs_runtime(&self) -> std::result::Result<(), Self::Error> {
        /*
                self.install_runtime(
                    include_directory!("runtime/golang/src/lcs"),
                    "src/lcs",
                )
        */
        Ok(())
    }
}
