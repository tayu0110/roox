use std::{
    collections::BTreeMap,
    io::{Read, Seek},
    str::FromStr,
};

use anyxml::{
    automata::xsregexp::XSRegexp,
    error::{XMLError, XMLErrorLevel},
    mediatype::MediaType,
    sax::{Attributes, EntityResolver, ErrorHandler, SAXHandler, XMLReader, error::SAXParseError},
    uri::{URIStr, URIString, escape_except, unescape},
};
use zip::{read::ZipFile, result::ZipError};

/// Media Types stream namespace.
///
/// # Reference
/// - ISO/IEC 29500-2:2021 Annex E
pub const MEDIA_TYPES_STREAM_NAMESPACE: &str =
    "http://schemas.openxmlformats.org/package/2006/content-types";
/// Core Properties namespace
///
/// # Reference
/// - ISO/IEC 29500-2:2021 Annex E
pub const CORE_PROPERTIES_NAMESPACE: &str =
    "http://schemas.openxmlformats.org/package/2006/metadata/core-properties";
/// Digital Signatures namespace
///
/// # Reference
/// - ISO/IEC 29500-2:2021 Annex E
pub const DIGITAL_SIGNATURES_NAMESPACE: &str =
    "http://schemas.openxmlformats.org/package/2006/digital-signature";
/// Relationships namespace
///
/// # Reference
/// - ISO/IEC 29500-2:2021 Annex E
pub const RELATIONSHIPS_NAMESPACE: &str =
    "http://schemas.openxmlformats.org/package/2006/relationships";

/// If `pack_uri` is a valid pack URI, return a pair of resolved package URI and part name.
///
/// # Reference
/// - ISO/IEC 29500-2:2021 6.3.3 Resolving a pack IRI to a resource
fn resolve_pack_uri(pack_uri: &URIStr) -> Option<(URIString, &str)> {
    let auth = pack_uri.authority()?.replace(",", "/");
    let auth = unescape(&auth).ok()?;
    let package_uri = URIString::parse(auth).ok()?;
    let path = pack_uri.path();
    Some((package_uri, path))
}

/// Compose `base_uri` to a pack schema URI.
///
/// `base_uri` must not be a relative URi. It can contain a fragment identifier.
///
/// # Reference
/// - ISO/IEC 29500-2:2021 6.3.4 Composing a pack IRI
fn compose_parck_uri(base_uri: &URIStr) -> URIString {
    // Remove fragment identifer
    let base_uri = base_uri.resolve(&URIString::parse("").unwrap()).to_string();
    let base_uri = escape_except(&base_uri, |c| !matches!(c, '%' | '?' | '@' | ':' | ','));
    let mut base_uri = base_uri.replace("/", ",");
    base_uri.insert_str(0, "pack://");
    base_uri.push('/');
    URIString::parse(base_uri).unwrap()
}

#[derive(Debug)]
pub enum PackageError {
    SchemaInvalidForContentTypes,
    SchemaInvalidForRelationships,
    BaseURINotAbsolute,
    XMLError(XMLError),
    ZipError(ZipError),
}

impl std::fmt::Display for PackageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::XMLError(e) => write!(f, "{e}"),
            Self::ZipError(e) => write!(f, "{e}"),
            Self::BaseURINotAbsolute => write!(f, "base URI is not absolute"),
            Self::SchemaInvalidForRelationships => write!(f, "schema invalid for relationships"),
            Self::SchemaInvalidForContentTypes => write!(f, "schema invalid for content-types"),
        }
    }
}

impl std::error::Error for PackageError {}

impl From<ZipError> for PackageError {
    fn from(value: ZipError) -> Self {
        Self::ZipError(value)
    }
}

impl From<XMLError> for PackageError {
    fn from(value: XMLError) -> Self {
        Self::XMLError(value)
    }
}

/// OPC package.
///
/// # Reference
/// - ISO/IEC 29500-2:2021
pub struct Package {
    /// Source parts
    parts: Vec<Part>,
    /// Root relationship part
    relation: Relationships,
    content_types: ContentTypes,
}

impl Package {
    pub fn from_reader<'a, R: Read + Seek + 'a>(
        reader: R,
        base_uri: &URIStr,
    ) -> Result<Self, PackageError> {
        if !base_uri.is_absolute() {
            return Err(PackageError::BaseURINotAbsolute);
        }
        let mut zip = zip::ZipArchive::new(reader)?;
        let content_types = zip.by_name("[Content_Types].xml")?;
        let content_types = ContentTypes::from_reader(content_types)?;
        let rel = zip.by_name("_rels/.rels")?;
        let relation = Relationships::from_reader(rel, base_uri)?;
        Ok(Package {
            parts: vec![],
            relation,
            content_types,
        })
    }
}

/// A source part.
struct Part {
    /// Pack scheme URI that specifies this part.
    uri: URIString,
    /// Media type of this source part.
    media_type: MediaType,
    /// Relationship of this source part.
    relation: Relationships,
    /// Source binary stream.
    source: Vec<u8>,
}

/// A relationship part.
struct Relationships {
    map: BTreeMap<String, Relationship>,
}

impl Relationships {
    fn from_reader<'a, R: Read>(
        reader: ZipFile<'a, R>,
        base_uri: &URIStr,
    ) -> Result<Self, PackageError> {
        let mut parser = XMLReader::builder()
            .set_handler(RelationshipBuildHandler {
                last_error: Ok(()),
                package_error: Ok(()),
                depth: 0,
                relationship: Relationships {
                    map: BTreeMap::new(),
                },
                base_uri: base_uri.into(),
            })
            .build();
        parser.parse_reader(reader, None, None)?;
        parser.handler.last_error.map_err(|e| e.error)?;
        parser.handler.package_error?;
        Ok(parser.handler.relationship)
    }
}

struct RelationshipBuildHandler {
    last_error: Result<(), SAXParseError>,
    package_error: Result<(), PackageError>,
    depth: usize,
    relationship: Relationships,
    base_uri: URIString,
}

impl ErrorHandler for RelationshipBuildHandler {
    fn fatal_error(&mut self, error: SAXParseError) {
        self.last_error = Err(error);
    }
    fn error(&mut self, error: SAXParseError) {
        if self
            .last_error
            .as_ref()
            .is_err_and(|e| matches!(e.level, XMLErrorLevel::Error | XMLErrorLevel::Warning))
        {
            self.last_error = Err(error);
        }
    }
    fn warning(&mut self, error: SAXParseError) {
        if self
            .last_error
            .as_ref()
            .is_err_and(|e| matches!(e.level, XMLErrorLevel::Warning))
        {
            self.last_error = Err(error);
        }
    }
}
impl EntityResolver for RelationshipBuildHandler {}
impl SAXHandler for RelationshipBuildHandler {
    fn start_element(
        &mut self,
        namespace_name: Option<&str>,
        local_name: Option<&str>,
        _qname: &str,
        atts: &Attributes,
    ) {
        self.depth += 1;
        if namespace_name != Some(RELATIONSHIPS_NAMESPACE) {
            self.package_error = Err(PackageError::SchemaInvalidForRelationships);
        }

        match local_name {
            Some("Relationships") => {
                if self.depth != 1 {
                    self.package_error = Err(PackageError::SchemaInvalidForRelationships);
                }
            }
            Some("Relationship") => {
                if self.depth != 2 {
                    self.package_error = Err(PackageError::SchemaInvalidForRelationships);
                    return;
                }
                let Some(target) = atts.get_value_by_qname("Target") else {
                    self.package_error = Err(PackageError::SchemaInvalidForRelationships);
                    return;
                };
                let Some(r#type) = atts.get_value_by_qname("Type") else {
                    self.package_error = Err(PackageError::SchemaInvalidForRelationships);
                    return;
                };
                let Some(id) = atts.get_value_by_qname("Id") else {
                    self.package_error = Err(PackageError::SchemaInvalidForRelationships);
                    return;
                };
                let Ok(target_mode) = atts
                    .get_value_by_qname("TargetMode")
                    .map(|m| m.parse::<TargetMode>())
                    .transpose()
                    .map(|m| m.unwrap_or_default())
                else {
                    self.package_error = Err(PackageError::SchemaInvalidForRelationships);
                    return;
                };

                match (URIString::parse(target), URIString::parse(r#type)) {
                    (Ok(target), Ok(r#type)) => {
                        let target = if matches!(target_mode, TargetMode::Internal) {
                            let packed = compose_parck_uri(&self.base_uri);
                            packed.resolve(&target).into()
                        } else {
                            self.base_uri.resolve(&target).into()
                        };
                        let rel = Relationship {
                            id: id.into(),
                            r#type: r#type.into(),
                            target,
                            target_mode,
                        };
                        // ID constraint
                        if self.relationship.map.insert(id.to_owned(), rel).is_some() {
                            self.package_error = Err(PackageError::SchemaInvalidForRelationships);
                        };
                    }
                    _ => {
                        self.package_error = Err(PackageError::SchemaInvalidForRelationships);
                    }
                }
            }
            _ => self.package_error = Err(PackageError::SchemaInvalidForRelationships),
        }
    }
    fn end_element(
        &mut self,
        _namespace_name: Option<&str>,
        _local_name: Option<&str>,
        _qname: &str,
    ) {
        self.depth -= 1;
    }
}

#[derive(Debug, Default)]
enum TargetMode {
    #[default]
    Internal,
    External,
}

impl FromStr for TargetMode {
    type Err = PackageError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Internal" => Ok(Self::Internal),
            "External" => Ok(Self::External),
            _ => Err(PackageError::SchemaInvalidForRelationships),
        }
    }
}

struct Relationship {
    id: Box<str>,
    r#type: Box<URIStr>,
    target: Box<URIStr>,
    target_mode: TargetMode,
}

struct ContentTypes {
    defaults: BTreeMap<String, MediaType>,
    overrides: BTreeMap<String, MediaType>,
}

impl ContentTypes {
    fn from_reader<'a, R: Read>(reader: ZipFile<'a, R>) -> Result<Self, PackageError> {
        let mut parser = XMLReader::builder()
            .set_handler(ContentTypesBuildHandler {
                last_error: Ok(()),
                package_error: Ok(()),
                depth: 0,
                extension_pattern: XSRegexp::compile(
                    "([!$&'\\(\\)\\*\\+,:=]|(%[0-9a-fA-F][0-9a-fA-F])|[:@]|[a-zA-Z0-9\\-_~])+",
                )
                .unwrap(),
                content_types: ContentTypes {
                    defaults: BTreeMap::new(),
                    overrides: BTreeMap::new(),
                },
            })
            .build();
        parser.parse_reader(reader, None, None)?;
        parser.handler.last_error.map_err(|e| e.error)?;
        parser.handler.package_error?;
        Ok(parser.handler.content_types)
    }
}

struct ContentTypesBuildHandler {
    last_error: Result<(), SAXParseError>,
    package_error: Result<(), PackageError>,
    depth: usize,
    extension_pattern: XSRegexp,
    content_types: ContentTypes,
}

impl ErrorHandler for ContentTypesBuildHandler {
    fn fatal_error(&mut self, error: SAXParseError) {
        self.last_error = Err(error);
    }
    fn error(&mut self, error: SAXParseError) {
        if self
            .last_error
            .as_ref()
            .is_err_and(|e| matches!(e.level, XMLErrorLevel::Error | XMLErrorLevel::Warning))
        {
            self.last_error = Err(error);
        }
    }
    fn warning(&mut self, error: SAXParseError) {
        if self
            .last_error
            .as_ref()
            .is_err_and(|e| matches!(e.level, XMLErrorLevel::Warning))
        {
            self.last_error = Err(error);
        }
    }
}
impl EntityResolver for ContentTypesBuildHandler {}
impl SAXHandler for ContentTypesBuildHandler {
    fn start_element(
        &mut self,
        namespace_name: Option<&str>,
        local_name: Option<&str>,
        _qname: &str,
        atts: &Attributes,
    ) {
        self.depth += 1;
        if namespace_name != Some(MEDIA_TYPES_STREAM_NAMESPACE) {
            self.package_error = Err(PackageError::SchemaInvalidForContentTypes);
        }

        match local_name {
            Some("Types") => {
                if self.depth != 1 {
                    self.package_error = Err(PackageError::SchemaInvalidForContentTypes);
                }
            }
            Some("Default") => {
                if self.depth != 2 {
                    self.package_error = Err(PackageError::SchemaInvalidForContentTypes);
                    return;
                }
                let Some(extension) = atts.get_value_by_qname("Extension") else {
                    self.package_error = Err(PackageError::SchemaInvalidForContentTypes);
                    return;
                };
                let Some(content_type) = atts.get_value_by_qname("ContentType") else {
                    self.package_error = Err(PackageError::SchemaInvalidForContentTypes);
                    return;
                };
                let ct = match content_type.parse::<MediaType>() {
                    Ok(ct) => ct,
                    Err(_) => {
                        self.package_error = Err(PackageError::SchemaInvalidForContentTypes);
                        return;
                    }
                };
                if !self.extension_pattern.is_match(extension) {
                    self.package_error = Err(PackageError::SchemaInvalidForContentTypes);
                    return;
                }
                self.content_types.defaults.insert(extension.to_owned(), ct);
            }
            Some("Override") => {
                if self.depth != 2 {
                    self.package_error = Err(PackageError::SchemaInvalidForContentTypes);
                    return;
                }
                let Some(content_type) = atts.get_value_by_qname("ContentType") else {
                    self.package_error = Err(PackageError::SchemaInvalidForContentTypes);
                    return;
                };
                let Some(part_name) = atts.get_value_by_qname("PartName") else {
                    self.package_error = Err(PackageError::SchemaInvalidForContentTypes);
                    return;
                };
                let ct = match content_type.parse::<MediaType>() {
                    Ok(ct) => ct,
                    Err(_) => {
                        self.package_error = Err(PackageError::SchemaInvalidForContentTypes);
                        return;
                    }
                };
                if !part_name.starts_with('/') || URIString::parse(content_type).is_err() {
                    self.package_error = Err(PackageError::SchemaInvalidForContentTypes);
                    return;
                }
                self.content_types
                    .overrides
                    .insert(part_name.to_owned(), ct);
            }
            _ => self.package_error = Err(PackageError::SchemaInvalidForContentTypes),
        }
    }
    fn end_element(
        &mut self,
        _namespace_name: Option<&str>,
        _local_name: Option<&str>,
        _qname: &str,
    ) {
        self.depth -= 1;
    }
}
