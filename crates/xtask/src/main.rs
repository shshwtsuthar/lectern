//! Repository maintenance commands.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    ffi::OsString,
    fmt::Write as _,
    fs::{self, File},
    io::{Cursor, Read},
    path::{Path, PathBuf},
    process::Command,
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use flate2::read::GzDecoder;
use serde::{Deserialize, Deserializer, de::MapAccess, de::Visitor};
use serde_json::Value;
use sha2::{Digest as _, Sha512};
use tar::Archive;
use tempfile::TempDir;

const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_ENTRY_BYTES: u64 = 16 * 1024 * 1024;

type Result<T> = std::result::Result<T, String>;

fn main() {
    if let Err(error) = run(env::args_os().skip(1)) {
        eprintln!("error: {error}");
        std::process::exit(2);
    }
}

fn run(arguments: impl IntoIterator<Item = OsString>) -> Result<()> {
    let mut arguments = arguments.into_iter();
    let Some(command) = arguments.next() else {
        return Err("expected a command; available command: primer-sync".into());
    };
    if command != "primer-sync" {
        return Err(format!(
            "unknown xtask command {}; available command: primer-sync",
            command.to_string_lossy()
        ));
    }
    primer_sync(&parse_primer_sync_arguments(arguments)?)
}

#[derive(Default)]
struct PrimerSyncArguments {
    check: bool,
    primitives_archive: Option<PathBuf>,
    tabler_icons_archive: Option<PathBuf>,
}

fn parse_primer_sync_arguments(
    arguments: impl IntoIterator<Item = OsString>,
) -> Result<PrimerSyncArguments> {
    let mut parsed = PrimerSyncArguments::default();
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--check") => parsed.check = true,
            Some("--primitives-archive") => {
                parsed.primitives_archive = Some(PathBuf::from(
                    arguments
                        .next()
                        .ok_or("--primitives-archive requires a path")?,
                ));
            }
            Some("--tabler-icons-archive") => {
                parsed.tabler_icons_archive = Some(PathBuf::from(
                    arguments
                        .next()
                        .ok_or("--tabler-icons-archive requires a path")?,
                ));
            }
            Some(value) => return Err(format!("unknown primer-sync argument {value:?}")),
            None => return Err("primer-sync arguments must be valid UTF-8".into()),
        }
    }
    Ok(parsed)
}

#[derive(Deserialize)]
struct SourceManifest {
    schema_version: u32,
    primer_primitives: PackageSource,
    tabler_icons: PackageSource,
    primer_react: GitSource,
    gpui: GitSource,
    gpui_platform: GitSource,
    gpui_base: GpuiBaseSource,
}

#[derive(Deserialize)]
struct PackageSource {
    package: String,
    version: String,
    archive_url: String,
    integrity: String,
}

#[derive(Deserialize)]
struct GitSource {
    git: String,
    rev: String,
}

#[derive(Deserialize)]
struct GpuiBaseSource {
    git: String,
    rev: String,
    resolved_gpui_rev: String,
}

#[derive(Deserialize)]
struct Allowlist {
    schema_version: u32,
    tokens: Vec<TokenSpec>,
    icons: Vec<IconSpec>,
}

#[derive(Clone, Deserialize)]
struct TokenSpec {
    name: String,
    rust_name: String,
    source: String,
    kind: String,
    scope: String,
}

#[derive(Clone, Deserialize)]
struct IconSpec {
    name: String,
    rust_name: String,
}

#[derive(Deserialize)]
struct PackageMetadata {
    name: String,
    version: String,
}

#[derive(Debug, Deserialize)]
struct RawToken {
    name: String,
    #[serde(rename = "type")]
    kind: String,
    value: Value,
}

struct GeneratedFile {
    relative_path: PathBuf,
    contents: Vec<u8>,
}

fn primer_sync(arguments: &PrimerSyncArguments) -> Result<()> {
    let repository = repository_root();
    let source_path = repository.join("tools/primer/primer-sources.toml");
    let allowlist_path = repository.join("tools/primer/token-allowlist.toml");
    let sources: SourceManifest = parse_toml(&source_path)?;
    let allowlist: Allowlist = parse_toml(&allowlist_path)?;
    validate_configuration(&sources, &allowlist)?;
    validate_cargo_lock(&repository.join("Cargo.lock"), &sources)?;

    let downloads =
        TempDir::new().map_err(|error| format!("create download directory: {error}"))?;
    let primitives_path = obtain_archive(
        arguments.primitives_archive.as_deref(),
        &sources.primer_primitives,
        downloads.path(),
        "primer-primitives.tgz",
    )?;
    let tabler_icons_path = obtain_archive(
        arguments.tabler_icons_archive.as_deref(),
        &sources.tabler_icons,
        downloads.path(),
        "tabler-icons.tgz",
    )?;

    verify_integrity(&primitives_path, &sources.primer_primitives)?;
    verify_integrity(&tabler_icons_path, &sources.tabler_icons)?;
    let generated = generate(&primitives_path, &tabler_icons_path, &sources, &allowlist)?;

    if arguments.check {
        check_generated(&repository, &generated)?;
        println!(
            "Primer UI sources are synchronized: primitives {}, Tabler Icons {}",
            sources.primer_primitives.version, sources.tabler_icons.version
        );
    } else {
        write_generated(&repository, &generated)?;
        println!(
            "Generated Primer UI sources from primitives {} and Tabler Icons {}",
            sources.primer_primitives.version, sources.tabler_icons.version
        );
    }
    Ok(())
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("xtask crate must be nested under the repository root")
        .to_path_buf()
}

fn parse_toml<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T> {
    let contents =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    toml::from_str(&contents).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn validate_configuration(sources: &SourceManifest, allowlist: &Allowlist) -> Result<()> {
    if sources.schema_version != 1 {
        return Err("tools/primer/primer-sources.toml schema_version must be 1".into());
    }
    if allowlist.schema_version != 1 {
        return Err("tools/primer/token-allowlist.toml schema_version must be 1".into());
    }
    if sources.gpui.git != sources.gpui_platform.git
        || sources.gpui.rev != sources.gpui_platform.rev
    {
        return Err("gpui and gpui_platform must use the same Git URL and revision".into());
    }
    if sources.gpui.rev != sources.gpui_base.resolved_gpui_rev {
        return Err("gpui-base resolved_gpui_rev must match the pinned GPUI revision".into());
    }
    validate_full_revision("primer_react", &sources.primer_react.rev)?;
    validate_full_revision("gpui", &sources.gpui.rev)?;
    validate_full_revision("gpui_platform", &sources.gpui_platform.rev)?;
    validate_full_revision("gpui_base", &sources.gpui_base.rev)?;
    validate_full_revision(
        "gpui_base.resolved_gpui_rev",
        &sources.gpui_base.resolved_gpui_rev,
    )?;
    if sources.primer_react.git.is_empty() || sources.gpui_base.git.is_empty() {
        return Err("pinned Git source URLs must not be empty".into());
    }
    if sources.primer_primitives.package != "@primer/primitives"
        || sources.tabler_icons.package != "@tabler/icons"
    {
        return Err(
            "package source identities must be @primer/primitives and @tabler/icons".into(),
        );
    }

    let mut rust_names = BTreeSet::new();
    let mut token_keys = BTreeSet::new();
    for token in &allowlist.tokens {
        if !rust_names.insert(token.rust_name.as_str()) {
            return Err(format!(
                "duplicate generated Rust token identifier {}",
                token.rust_name
            ));
        }
        let key = (&token.scope, &token.source, &token.name);
        if !token_keys.insert(key) {
            return Err(format!("duplicate allowlisted token {}", token.name));
        }
        if !is_rust_constant(&token.rust_name) {
            return Err(format!(
                "token {} has invalid Rust constant identifier {}",
                token.name, token.rust_name
            ));
        }
        if !matches!(token.scope.as_str(), "theme" | "common") {
            return Err(format!(
                "token {} has unsupported scope {}",
                token.name, token.scope
            ));
        }
        if !matches!(
            token.kind.as_str(),
            "color" | "dimension" | "number" | "fontWeight"
        ) {
            return Err(format!(
                "token {} has unsupported expected type {}",
                token.name, token.kind
            ));
        }
        if token.scope == "theme" && token.source != "functional/themes" {
            return Err(format!(
                "theme token {} must use functional/themes",
                token.name
            ));
        }
        if token.scope == "common" && (token.source.starts_with('/') || token.source.contains(".."))
        {
            return Err(format!("token {} has unsafe source path", token.name));
        }
    }

    let mut icon_names = BTreeSet::new();
    let mut icon_rust_names = BTreeSet::new();
    for icon in &allowlist.icons {
        if !icon_names.insert(icon.name.as_str()) {
            return Err(format!("duplicate allowlisted icon {}", icon.name));
        }
        if !icon_rust_names.insert(icon.rust_name.as_str()) {
            return Err(format!(
                "duplicate generated Rust icon identifier {}",
                icon.rust_name
            ));
        }
        if !is_rust_variant(&icon.rust_name) {
            return Err(format!(
                "icon {} has invalid Rust variant identifier {}",
                icon.name, icon.rust_name
            ));
        }
    }
    Ok(())
}

fn validate_cargo_lock(path: &Path, sources: &SourceManifest) -> Result<()> {
    let contents = fs::read_to_string(path)
        .map_err(|error| format!("read pinned dependency graph {}: {error}", path.display()))?;
    let lock: toml::Value = toml::from_str(&contents)
        .map_err(|error| format!("parse pinned dependency graph {}: {error}", path.display()))?;
    let packages = lock
        .get("package")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| format!("{} has no package array", path.display()))?;
    let mut gpui_found = false;
    let mut gpui_platform_found = false;
    let mut gpui_base_found = false;
    let zed_prefix = format!("git+{}", sources.gpui.git);
    let expected_zed_suffix = format!("#{}", sources.gpui.rev);
    let gpui_base_prefix = format!(
        "git+{}?rev={}",
        sources.gpui_base.git, sources.gpui_base.rev
    );
    let expected_gpui_base_suffix = format!("#{}", sources.gpui_base.rev);

    for package in packages {
        let name = package
            .get("name")
            .and_then(toml::Value::as_str)
            .unwrap_or_default();
        let Some(source) = package.get("source").and_then(toml::Value::as_str) else {
            continue;
        };
        if source.starts_with(&zed_prefix) && !source.ends_with(&expected_zed_suffix) {
            return Err(format!(
                "Cargo.lock resolves {name} from {source}, not pinned GPUI revision {}",
                sources.gpui.rev
            ));
        }
        if source.starts_with(&gpui_base_prefix) && !source.ends_with(&expected_gpui_base_suffix) {
            return Err(format!(
                "Cargo.lock resolves {name} from {source}, not pinned gpui-base revision {}",
                sources.gpui_base.rev
            ));
        }
        if name == "gpui" && source.starts_with(&zed_prefix) {
            gpui_found = true;
        }
        if name == "gpui_platform" && source.starts_with(&zed_prefix) {
            gpui_platform_found = true;
        }
        if name == "gpui-base" && source.starts_with(&gpui_base_prefix) {
            gpui_base_found = true;
        }
    }
    if !gpui_found || !gpui_platform_found || !gpui_base_found {
        return Err(
            "Cargo.lock must resolve pinned gpui, gpui_platform, and gpui-base packages".into(),
        );
    }
    Ok(())
}

fn validate_full_revision(label: &str, revision: &str) -> Result<()> {
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("{label} must use a full 40-character Git revision"));
    }
    Ok(())
}

fn is_rust_constant(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        && value.as_bytes()[0].is_ascii_uppercase()
}

fn is_rust_variant(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && value.as_bytes()[0].is_ascii_uppercase()
}

fn obtain_archive(
    explicit: Option<&Path>,
    source: &PackageSource,
    directory: &Path,
    filename: &str,
) -> Result<PathBuf> {
    if let Some(path) = explicit {
        if !path.is_file() {
            return Err(format!(
                "{} {} archive does not exist: {}",
                source.package,
                source.version,
                path.display()
            ));
        }
        return Ok(path.to_path_buf());
    }

    let destination = directory.join(filename);
    let status = Command::new("curl")
        .args(["--fail", "--location", "--silent", "--show-error"])
        .arg("--output")
        .arg(&destination)
        .arg(&source.archive_url)
        .status()
        .map_err(|error| {
            format!(
                "download {} {} with curl (or pass an explicit archive): {error}",
                source.package, source.version
            )
        })?;
    if !status.success() {
        return Err(format!(
            "download {} {} from {} failed with {status}",
            source.package, source.version, source.archive_url
        ));
    }
    Ok(destination)
}

fn verify_integrity(path: &Path, source: &PackageSource) -> Result<()> {
    let metadata =
        fs::metadata(path).map_err(|error| format!("inspect {}: {error}", path.display()))?;
    if metadata.len() > MAX_ARCHIVE_BYTES {
        return Err(format!(
            "{} {} archive is {} bytes, above the {} byte cap",
            source.package,
            source.version,
            metadata.len(),
            MAX_ARCHIVE_BYTES
        ));
    }
    let expected = source.integrity.strip_prefix("sha512-").ok_or_else(|| {
        format!(
            "{} {} integrity must be a sha512 SRI value",
            source.package, source.version
        )
    })?;
    let bytes = fs::read(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    let actual = BASE64.encode(Sha512::digest(&bytes));
    if actual != expected {
        return Err(format!(
            "{} {} archive integrity mismatch for {}: expected sha512-{expected}, got sha512-{actual}",
            source.package,
            source.version,
            path.display()
        ));
    }
    Ok(())
}

fn generate(
    primitives_path: &Path,
    tabler_icons_path: &Path,
    sources: &SourceManifest,
    allowlist: &Allowlist,
) -> Result<Vec<GeneratedFile>> {
    let primitive_paths = primitive_archive_paths(allowlist);
    let primitives = read_archive_entries(primitives_path, &primitive_paths)?;
    verify_package(&primitives, &sources.primer_primitives)?;

    let tabler_icon_paths = tabler_icon_archive_paths(allowlist);
    let tabler_icons = read_archive_entries(tabler_icons_path, &tabler_icon_paths)?;
    verify_package(&tabler_icons, &sources.tabler_icons)?;

    let mut parsed_inputs = BTreeMap::new();
    for path in primitive_paths.iter().filter(|path| {
        path.starts_with("package/dist/docs/")
            && Path::new(path.as_str())
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
    }) {
        let bytes = primitives
            .get(path)
            .ok_or_else(|| format!("missing required primitives entry {path}"))?;
        parsed_inputs.insert(path.clone(), parse_unique_token_object(path, bytes)?);
    }

    let mut files = vec![
        generated_text(
            "crates/lectern-ui/src/generated/mod.rs",
            render_generated_mod(sources),
        ),
        generated_text(
            "crates/lectern-ui/src/generated/primitive_metadata.rs",
            render_common_tokens(sources, allowlist, &parsed_inputs)?,
        ),
        generated_text(
            "crates/lectern-ui/src/generated/light.rs",
            render_theme_tokens("light", sources, allowlist, &parsed_inputs)?,
        ),
        generated_text(
            "crates/lectern-ui/src/generated/dark.rs",
            render_theme_tokens("dark", sources, allowlist, &parsed_inputs)?,
        ),
        generated_text(
            "crates/lectern-ui/src/generated/tabler_icons.rs",
            render_tabler_icons(sources, allowlist),
        ),
        generated_text(
            "third_party/primer-primitives/PROVENANCE.md",
            render_package_provenance("Primer Primitives", &sources.primer_primitives),
        ),
        generated_text(
            "third_party/tabler-icons/PROVENANCE.md",
            render_package_provenance("Tabler Icons", &sources.tabler_icons),
        ),
    ];
    files.push(generated_bytes(
        "third_party/primer-primitives/LICENSE",
        archive_entry(&primitives, "package/LICENSE")?.to_vec(),
    ));
    files.push(generated_bytes(
        "third_party/tabler-icons/LICENSE",
        archive_entry(&tabler_icons, "package/LICENSE")?.to_vec(),
    ));
    for icon in &allowlist.icons {
        let archive_path = format!("package/icons/outline/{}.svg", icon.name);
        files.push(generated_bytes(
            format!("crates/lectern-ui/assets/tabler/{}.svg", icon.name),
            archive_entry(&tabler_icons, &archive_path)?.to_vec(),
        ));
    }
    files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    Ok(files)
}

fn primitive_archive_paths(allowlist: &Allowlist) -> BTreeSet<String> {
    let mut paths = BTreeSet::from(["package/package.json".into(), "package/LICENSE".into()]);
    for token in &allowlist.tokens {
        if token.scope == "theme" {
            paths.insert("package/dist/docs/functional/themes/light.json".into());
            paths.insert("package/dist/docs/functional/themes/dark.json".into());
        } else {
            paths.insert(format!("package/dist/docs/{}", token.source));
        }
    }
    paths
}

fn tabler_icon_archive_paths(allowlist: &Allowlist) -> BTreeSet<String> {
    let mut paths = BTreeSet::from(["package/package.json".into(), "package/LICENSE".into()]);
    for icon in &allowlist.icons {
        paths.insert(format!("package/icons/outline/{}.svg", icon.name));
    }
    paths
}

fn read_archive_entries(
    path: &Path,
    expected: &BTreeSet<String>,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let file = File::open(path).map_err(|error| format!("open {}: {error}", path.display()))?;
    let mut archive = Archive::new(GzDecoder::new(file));
    let mut found = BTreeMap::new();
    let entries = archive
        .entries()
        .map_err(|error| format!("read archive {}: {error}", path.display()))?;
    for entry in entries {
        let mut entry = entry.map_err(|error| format!("read {} entry: {error}", path.display()))?;
        let entry_path = entry
            .path()
            .map_err(|error| format!("read {} entry path: {error}", path.display()))?
            .to_str()
            .ok_or_else(|| format!("{} contains a non-UTF-8 path", path.display()))?
            .to_owned();
        if !expected.contains(&entry_path) {
            continue;
        }
        if entry.size() > MAX_ENTRY_BYTES {
            return Err(format!(
                "{} entry {entry_path} is {} bytes, above the {} byte cap",
                path.display(),
                entry.size(),
                MAX_ENTRY_BYTES
            ));
        }
        let capacity = usize::try_from(entry.size()).map_err(|error| {
            format!(
                "{} entry {entry_path} size cannot fit in memory: {error}",
                path.display()
            )
        })?;
        let mut bytes = Vec::with_capacity(capacity);
        entry
            .read_to_end(&mut bytes)
            .map_err(|error| format!("read {} entry {entry_path}: {error}", path.display()))?;
        if found.insert(entry_path.clone(), bytes).is_some() {
            return Err(format!(
                "{} repeats archive entry {entry_path}",
                path.display()
            ));
        }
    }
    let missing: Vec<_> = expected
        .iter()
        .filter(|entry| !found.contains_key(*entry))
        .cloned()
        .collect();
    if !missing.is_empty() {
        return Err(format!(
            "{} is missing required entries: {}",
            path.display(),
            missing.join(", ")
        ));
    }
    Ok(found)
}

fn verify_package(entries: &BTreeMap<String, Vec<u8>>, source: &PackageSource) -> Result<()> {
    let package: PackageMetadata =
        serde_json::from_slice(archive_entry(entries, "package/package.json")?).map_err(
            |error| {
                format!(
                    "parse {} {} package.json: {error}",
                    source.package, source.version
                )
            },
        )?;
    if package.name != source.package || package.version != source.version {
        return Err(format!(
            "archive package identity mismatch: expected {} {}, got {} {}",
            source.package, source.version, package.name, package.version
        ));
    }
    Ok(())
}

fn archive_entry<'a>(entries: &'a BTreeMap<String, Vec<u8>>, path: &str) -> Result<&'a [u8]> {
    entries
        .get(path)
        .map(Vec::as_slice)
        .ok_or_else(|| format!("archive did not retain required entry {path}"))
}

struct UniqueTokenObject;

impl<'de> Visitor<'de> for UniqueTokenObject {
    type Value = BTreeMap<String, Value>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("an object with unique token names")
    }

    fn visit_map<A: MapAccess<'de>>(
        self,
        mut access: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let mut values = BTreeMap::new();
        while let Some((key, value)) = access.next_entry::<String, Value>()? {
            if values.insert(key.clone(), value).is_some() {
                return Err(serde::de::Error::custom(format!(
                    "duplicate token name {key}"
                )));
            }
        }
        Ok(values)
    }
}

impl<'de> Deserialize<'de> for UniqueTokenObject {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let _ = deserializer;
        unreachable!("UniqueTokenObject is a visitor marker")
    }
}

fn parse_unique_token_object(path: &str, bytes: &[u8]) -> Result<BTreeMap<String, RawToken>> {
    let mut deserializer = serde_json::Deserializer::from_reader(Cursor::new(bytes));
    let values = deserializer
        .deserialize_map(UniqueTokenObject)
        .map_err(|error| format!("parse @primer/primitives 11.10.0 {path}: {error}"))?;
    deserializer
        .end()
        .map_err(|error| format!("parse @primer/primitives 11.10.0 {path}: {error}"))?;
    let mut tokens = BTreeMap::new();
    for (key, value) in values {
        let token: RawToken = serde_json::from_value(value).map_err(|error| {
            format!("parse @primer/primitives 11.10.0 {path} token {key}: {error}")
        })?;
        if token.name != key {
            return Err(format!(
                "@primer/primitives 11.10.0 {path} key {key} disagrees with token name {}",
                token.name
            ));
        }
        tokens.insert(key, token);
    }
    Ok(tokens)
}

fn render_generated_mod(sources: &SourceManifest) -> String {
    format!(
        "{}pub(crate) mod dark;\npub(crate) mod light;\npub(crate) mod primitive_metadata;\npub(crate) mod tabler_icons;\n",
        generated_header(sources, "module registry")
    )
}

fn render_common_tokens(
    sources: &SourceManifest,
    allowlist: &Allowlist,
    inputs: &BTreeMap<String, BTreeMap<String, RawToken>>,
) -> Result<String> {
    let mut output = generated_header(sources, "common token allowlist");
    let summary = format!(
        "@primer/primitives {}; @tabler/icons {}; Primer React {}; GPUI {}; gpui-base {}",
        sources.primer_primitives.version,
        sources.tabler_icons.version,
        sources.primer_react.rev,
        sources.gpui.rev,
        sources.gpui_base.rev
    );
    writeln!(
        output,
        "pub(crate) const SOURCE_SUMMARY: &str = {summary:?};\n"
    )
    .expect("writing to a String cannot fail");
    let mut tokens: Vec<_> = allowlist
        .tokens
        .iter()
        .filter(|token| token.scope == "common")
        .collect();
    tokens.sort_by_key(|token| &token.rust_name);
    for spec in tokens {
        let path = format!("package/dist/docs/{}", spec.source);
        let token = resolve_token(inputs, &path, spec)?;
        output.push_str(&render_token_constant(spec, token)?);
    }
    Ok(output)
}

fn render_theme_tokens(
    theme: &str,
    sources: &SourceManifest,
    allowlist: &Allowlist,
    inputs: &BTreeMap<String, BTreeMap<String, RawToken>>,
) -> Result<String> {
    let path = format!("package/dist/docs/functional/themes/{theme}.json");
    let mut output = generated_header(sources, &path);
    let mut tokens: Vec<_> = allowlist
        .tokens
        .iter()
        .filter(|token| token.scope == "theme")
        .collect();
    tokens.sort_by_key(|token| &token.rust_name);
    for spec in tokens {
        let token = resolve_token(inputs, &path, spec)?;
        output.push_str(&render_token_constant(spec, token)?);
    }
    Ok(output)
}

fn resolve_token<'a>(
    inputs: &'a BTreeMap<String, BTreeMap<String, RawToken>>,
    path: &str,
    spec: &TokenSpec,
) -> Result<&'a RawToken> {
    let token = inputs
        .get(path)
        .and_then(|tokens| tokens.get(&spec.name))
        .ok_or_else(|| {
            format!(
                "@primer/primitives token {} is missing from {path}",
                spec.name
            )
        })?;
    if token.kind != spec.kind {
        return Err(format!(
            "@primer/primitives token {} in {path} has type {}, expected {}",
            spec.name, token.kind, spec.kind
        ));
    }
    Ok(token)
}

fn render_token_constant(spec: &TokenSpec, token: &RawToken) -> Result<String> {
    let (rust_type, value) = match spec.kind.as_str() {
        "color" => {
            let value = parse_color(&token.value, spec)?;
            (
                "u32",
                format!("0x{:04x}_{:04x}", value >> 16, value & 0xffff),
            )
        }
        "dimension" => ("f32", format_float(parse_rem(&token.value, spec)?)),
        "number" => ("f32", format_float(parse_number(&token.value, spec)?)),
        "fontWeight" => ("u16", parse_font_weight(&token.value, spec)?.to_string()),
        _ => {
            return Err(format!(
                "token {} has unsupported generator type {}",
                spec.name, spec.kind
            ));
        }
    };
    Ok(format!(
        "pub(crate) const {}: {rust_type} = {value};\n",
        spec.rust_name
    ))
}

fn string_value<'a>(value: &'a Value, spec: &TokenSpec) -> Result<&'a str> {
    value.as_str().ok_or_else(|| {
        format!(
            "@primer/primitives token {} expected a {} string, got {value}",
            spec.name, spec.kind
        )
    })
}

fn parse_color(value: &Value, spec: &TokenSpec) -> Result<u32> {
    let value = string_value(value, spec)?;
    let digits = value.strip_prefix('#').ok_or_else(|| {
        format!(
            "@primer/primitives token {} has unsupported color {value:?}",
            spec.name
        )
    })?;
    let parsed = u32::from_str_radix(digits, 16).map_err(|_| {
        format!(
            "@primer/primitives token {} has invalid color {value:?}",
            spec.name
        )
    })?;
    match digits.len() {
        6 => Ok((parsed << 8) | 0xff),
        8 => Ok(parsed),
        _ => Err(format!(
            "@primer/primitives token {} has unsupported color {value:?}",
            spec.name
        )),
    }
}

fn parse_rem(value: &Value, spec: &TokenSpec) -> Result<f32> {
    let value = string_value(value, spec)?;
    let number = value.strip_suffix("rem").ok_or_else(|| {
        format!(
            "@primer/primitives token {} has unsupported dimension {value:?}; expected rem",
            spec.name
        )
    })?;
    parse_finite(number, spec)
}

fn parse_number(value: &Value, spec: &TokenSpec) -> Result<f32> {
    let number = value.as_f64().ok_or_else(|| {
        format!(
            "@primer/primitives token {} expected a number, got {value}",
            spec.name
        )
    })?;
    let number: f32 = number
        .to_string()
        .parse()
        .map_err(|error| format!("number conversion failed for token {}: {error}", spec.name))?;
    if !number.is_finite() || number < 0.0 {
        return Err(format!(
            "@primer/primitives token {} has out-of-range number {value}",
            spec.name
        ));
    }
    Ok(number)
}

fn parse_font_weight(value: &Value, spec: &TokenSpec) -> Result<u16> {
    let weight = value.as_u64().ok_or_else(|| {
        format!(
            "@primer/primitives token {} expected an integer font weight, got {value}",
            spec.name
        )
    })?;
    if !(1..=1_000).contains(&weight) {
        return Err(format!(
            "@primer/primitives token {} has out-of-range font weight {weight}",
            spec.name
        ));
    }
    u16::try_from(weight).map_err(|error| format!("font weight conversion failed: {error}"))
}

fn parse_finite(value: &str, spec: &TokenSpec) -> Result<f32> {
    let number: f32 = value.parse().map_err(|_| {
        format!(
            "@primer/primitives token {} has invalid numeric value {value:?}",
            spec.name
        )
    })?;
    if !number.is_finite() {
        return Err(format!(
            "@primer/primitives token {} has non-finite numeric value {value:?}",
            spec.name
        ));
    }
    Ok(number)
}

fn format_float(value: f32) -> String {
    let mut rendered = format!("{value:.8}");
    while rendered.ends_with('0') {
        rendered.pop();
    }
    if rendered.ends_with('.') {
        rendered.push('0');
    }
    rendered
}

fn render_tabler_icons(sources: &SourceManifest, allowlist: &Allowlist) -> String {
    let mut icons = allowlist.icons.clone();
    icons.sort_by_key(|icon| icon.rust_name.clone());
    let mut output = generated_header(sources, "selected Tabler Icon assets");
    output.push_str(
        "/// A Tabler outline icon vendored for a committed Lectern control.\n#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]\npub enum TablerIcon {\n",
    );
    for icon in &icons {
        write!(
            output,
            "    /// The `{}` Tabler icon.\n    {},\n",
            icon.name, icon.rust_name
        )
        .expect("writing to a String cannot fail");
    }
    output.push_str(
        "}\n\nimpl TablerIcon {\n    /// Returns the static GPUI asset path for this icon.\n    #[must_use]\n    pub const fn path(self) -> &'static str {\n        match self {\n",
    );
    for icon in &icons {
        writeln!(
            output,
            "            Self::{} => \"tabler/{}.svg\",",
            icon.rust_name, icon.name
        )
        .expect("writing to a String cannot fail");
    }
    output.push_str("        }\n    }\n}\n");
    output
}

fn generated_header(sources: &SourceManifest, inputs: &str) -> String {
    format!(
        "// @generated by `cargo xtask primer-sync`; do not edit.\n// @primer/primitives {} ({})\n// @tabler/icons {} ({})\n// Generator schema: 1; inputs: {inputs}\n\n",
        sources.primer_primitives.version,
        sources.primer_primitives.integrity,
        sources.tabler_icons.version,
        sources.tabler_icons.integrity
    )
}

fn render_package_provenance(title: &str, source: &PackageSource) -> String {
    format!(
        "# {title} provenance\n\n- Package: `{}`\n- Version: `{}`\n- Archive: `{}`\n- Integrity: `{}`\n- Generated by: `cargo xtask primer-sync`\n",
        source.package, source.version, source.archive_url, source.integrity
    )
}

fn generated_text(path: impl Into<PathBuf>, contents: String) -> GeneratedFile {
    generated_bytes(path, contents.into_bytes())
}

fn generated_bytes(path: impl Into<PathBuf>, contents: Vec<u8>) -> GeneratedFile {
    GeneratedFile {
        relative_path: path.into(),
        contents,
    }
}

fn write_generated(repository: &Path, generated: &[GeneratedFile]) -> Result<()> {
    for file in generated {
        let path = repository.join(&file.relative_path);
        let parent = path
            .parent()
            .ok_or_else(|| format!("generated path {} has no parent", path.display()))?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
        fs::write(&path, &file.contents)
            .map_err(|error| format!("write {}: {error}", path.display()))?;
    }
    Ok(())
}

fn check_generated(repository: &Path, generated: &[GeneratedFile]) -> Result<()> {
    let temporary = TempDir::new().map_err(|error| format!("create check directory: {error}"))?;
    write_generated(temporary.path(), generated)?;
    let mut differences = Vec::new();
    for file in generated {
        let checked_in = repository.join(&file.relative_path);
        let expected = temporary.path().join(&file.relative_path);
        let expected_bytes = fs::read(&expected)
            .map_err(|error| format!("read generated {}: {error}", expected.display()))?;
        match fs::read(&checked_in) {
            Ok(actual) if actual == expected_bytes => {}
            Ok(_) => differences.push(format!("changed {}", file.relative_path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                differences.push(format!("missing {}", file.relative_path.display()));
            }
            Err(error) => return Err(format!("read {}: {error}", checked_in.display())),
        }
    }
    if differences.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "checked-in Primer output is stale: {}; run `cargo xtask primer-sync`",
            differences.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(name: &str, kind: &str) -> TokenSpec {
        TokenSpec {
            name: name.into(),
            rust_name: "TOKEN".into(),
            source: "fixture.json".into(),
            kind: kind.into(),
            scope: "common".into(),
        }
    }

    #[test]
    fn parses_opaque_and_alpha_colors() {
        assert_eq!(
            parse_color(&Value::String("#0969da".into()), &token("color", "color")).unwrap(),
            0x0969_daff
        );
        assert_eq!(
            parse_color(&Value::String("#1f232826".into()), &token("color", "color")).unwrap(),
            0x1f23_2826
        );
    }

    #[test]
    fn preserves_negative_rem_dimensions() {
        let parsed = parse_rem(
            &Value::String("-0.125rem".into()),
            &token("offset", "dimension"),
        )
        .unwrap();
        assert!((parsed + 0.125).abs() < f32::EPSILON);
    }

    #[test]
    fn duplicate_json_tokens_fail_closed() {
        let error = parse_unique_token_object(
            "fixture.json",
            br#"{"same":{"name":"same","type":"number","value":1},"same":{"name":"same","type":"number","value":2}}"#,
        )
        .unwrap_err();
        assert!(error.contains("duplicate token name same"));
    }

    #[test]
    fn key_and_declared_name_must_agree() {
        let error = parse_unique_token_object(
            "fixture.json",
            br#"{"key":{"name":"different","type":"number","value":1}}"#,
        )
        .unwrap_err();
        assert!(error.contains("disagrees with token name different"));
    }
}
