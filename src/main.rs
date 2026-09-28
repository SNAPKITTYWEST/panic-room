// panic-room/src/main.rs
//
// PANIC ROOM
// Semantic continuity engine for Rust repositories.
//
// Pipeline:
//
// Git diff
//   -> Rust source analysis
//   -> SemanticUnit[]
//   -> SemanticGraph
//   -> SemanticSnapshot
//   -> deterministic bi-encoder
//   -> optional DSPy HTTP analysis
//   -> ContinuityEngine
//   -> PASS / PANIC
//
// This is an executable Rust implementation, not a scaffold.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    env,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};
use syn::{
    visit::Visit,
    ExprCall, ExprMethodCall, Fields, ImplItem, Item, ItemEnum, ItemFn, ItemImpl,
    ItemStruct, ItemTrait, Pat, ReturnType, Type,
};
use walkdir::WalkDir;

// ============================================================
// BASIC TYPES
// ============================================================

type UnitId = String;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum UnitKind {
    Module,
    Struct,
    Enum,
    Trait,
    Impl,
    Function,
    Method,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum EdgeKind {
    Calls,
    Owns,
    Contains,
    Implements,
    DependsOn,
    Returns,
    Accepts,
    References,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Parameter {
    pub name: String,
    pub ty: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldInfo {
    pub name: String,
    pub ty: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticUnit {
    pub id: UnitId,
    pub name: String,
    pub kind: UnitKind,
    pub module: String,
    pub signature: String,

    pub parameters: Vec<Parameter>,
    pub return_type: Option<String>,
    pub fields: Vec<FieldInfo>,

    pub calls: Vec<String>,
    pub references: Vec<String>,
    pub traits: Vec<String>,
    pub dependencies: Vec<String>,

    pub documentation: String,
    pub source_path: String,
    pub source_hash: String,
    pub source_start: usize,
    pub source_end: usize,

    pub representation: String,
    pub embedding: Vec<f32>,
}

impl SemanticUnit {
    fn new(
        id: impl Into<String>,
        name: impl Into<String>,
        kind: UnitKind,
        module: impl Into<String>,
        source_path: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            kind,
            module: module.into(),
            signature: String::new(),
            parameters: Vec::new(),
            return_type: None,
            fields: Vec::new(),
            calls: Vec::new(),
            references: Vec::new(),
            traits: Vec::new(),
            dependencies: Vec::new(),
            documentation: String::new(),
            source_path: source_path.into(),
            source_hash: String::new(),
            source_start: 0,
            source_end: 0,
            representation: String::new(),
            embedding: Vec::new(),
        }
    }

    fn canonicalize(&mut self) {
        self.calls.sort();
        self.calls.dedup();

        self.references.sort();
        self.references.dedup();

        self.traits.sort();
        self.traits.dedup();

        self.dependencies.sort();
        self.dependencies.dedup();

        self.parameters.sort_by(|a, b| a.name.cmp(&b.name));
        self.fields.sort_by(|a, b| a.name.cmp(&b.name));

        self.representation = canonical_representation(self);
        self.embedding = encode_text(&self.representation);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticEdge {
    pub from: UnitId,
    pub to: UnitId,
    pub kind: EdgeKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SemanticGraph {
    pub nodes: BTreeMap<UnitId, SemanticUnit>,
    pub edges: Vec<SemanticEdge>,
}

impl SemanticGraph {
    pub fn add_node(&mut self, unit: SemanticUnit) {
        self.nodes.insert(unit.id.clone(), unit);
    }

    pub fn add_edge(
        &mut self,
        from: impl Into<String>,
        to: impl Into<String>,
        kind: EdgeKind,
    ) {
        let edge = SemanticEdge {
            from: from.into(),
            to: to.into(),
            kind,
        };

        if !self.edges.iter().any(|x| {
            x.from == edge.from &&
            x.to == edge.to &&
            x.kind == edge.kind
        }) {
            self.edges.push(edge);
        }
    }

    pub fn remove_node(&mut self, id: &str) {
        self.nodes.remove(id);
        self.edges.retain(|e| e.from != id && e.to != id);
    }

    pub fn neighbors(&self, id: &str) -> Vec<&SemanticUnit> {
        let mut result = Vec::new();

        for edge in &self.edges {
            if edge.from == id {
                if let Some(unit) = self.nodes.get(&edge.to) {
                    result.push(unit);
                }
            }

            if edge.to == id {
                if let Some(unit) = self.nodes.get(&edge.from) {
                    result.push(unit);
                }
            }
        }

        result
    }

    pub fn dependencies(&self, id: &str) -> Vec<&SemanticUnit> {
        self.edges
            .iter()
            .filter(|e| e.from == id)
            .filter_map(|e| self.nodes.get(&e.to))
            .collect()
    }

    pub fn dependents(&self, id: &str) -> Vec<&SemanticUnit> {
        self.edges
            .iter()
            .filter(|e| e.to == id)
            .filter_map(|e| self.nodes.get(&e.from))
            .collect()
    }

    pub fn affected_units(&self, roots: &[UnitId]) -> BTreeSet<UnitId> {
        let mut seen = BTreeSet::new();
        let mut queue = VecDeque::new();

        for root in roots {
            queue.push_back(root.clone());
            seen.insert(root.clone());
        }

        while let Some(current) = queue.pop_front() {
            for edge in &self.edges {
                if edge.from == current && seen.insert(edge.to.clone()) {
                    queue.push_back(edge.to.clone());
                }

                if edge.to == current && seen.insert(edge.from.clone()) {
                    queue.push_back(edge.from.clone());
                }
            }
        }

        seen
    }
}

// ============================================================
// SNAPSHOT
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contract {
    pub subject: String,
    pub statement: String,
    pub category: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticSnapshot {
    pub commit: String,
    pub repository: String,
    pub units: BTreeMap<UnitId, SemanticUnit>,
    pub graph: SemanticGraph,
    pub contracts: Vec<Contract>,
    pub repository_embedding: Vec<f32>,
}

impl SemanticSnapshot {
    pub fn save(&self, root: &Path) -> io::Result<()> {
        let dir = root.join(".panic-room").join("snapshots");
        fs::create_dir_all(&dir)?;

        let filename = format!("{}.json", self.commit);
        let path = dir.join(filename);

        let data = serde_json::to_vec_pretty(self)
            .map_err(io::Error::other)?;

        fs::write(path, data)
    }

    pub fn load(root: &Path, commit: &str) -> io::Result<Self> {
        let path = root
            .join(".panic-room")
            .join("snapshots")
            .join(format!("{commit}.json"));

        let data = fs::read(path)?;

        serde_json::from_slice(&data)
            .map_err(io::Error::other)
    }
}

// ============================================================
// RUST PARSER
// ============================================================

struct RustVisitor {
    path: String,
    module: String,
    source: String,
    units: Vec<SemanticUnit>,
    current_impl: Option<String>,
}

impl RustVisitor {
    fn new(path: String, module: String, source: String) -> Self {
        Self {
            path,
            module,
            source,
            units: Vec::new(),
            current_impl: None,
        }
    }

    fn make_id(&self, kind: &str, name: &str) -> String {
        if self.module.is_empty() {
            format!("{kind}::{name}")
        } else {
            format!("{kind}::{}::{name}", self.module)
        }
    }

    fn finalize(&mut self, unit: &mut SemanticUnit) {
        unit.source_hash = sha256(&self.source);
        unit.source_start = 0;
        unit.source_end = self.source.len();
        unit.canonicalize();
    }

    fn documentation(attrs: &[syn::Attribute]) -> String {
        attrs.iter()
            .filter_map(|a| {
                if !a.path().is_ident("doc") {
                    return None;
                }

                let meta = a.meta.to_token_stream().to_string();
                Some(meta)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl<'ast> Visit<'ast> for RustVisitor {
    fn visit_item_struct(&mut self, node: &'ast ItemStruct) {
        let name = node.ident.to_string();

        let mut unit = SemanticUnit::new(
            self.make_id("struct", &name),
            name,
            UnitKind::Struct,
            self.module.clone(),
            self.path.clone(),
        );

        unit.documentation = Self::documentation(&node.attrs);

        match &node.fields {
            Fields::Named(fields) => {
                for field in &fields.named {
                    if let Some(field_name) = &field.ident {
                        unit.fields.push(FieldInfo {
                            name: field_name.to_string(),
                            ty: type_string(&field.ty),
                        });
                    }
                }
            }

            Fields::Unnamed(fields) => {
                for (index, field) in fields.unnamed.iter().enumerate() {
                    unit.fields.push(FieldInfo {
                        name: format!("_{index}"),
                        ty: type_string(&field.ty),
                    });
                }
            }

            Fields::Unit => {}
        }

        self.finalize(&mut unit);
        self.units.push(unit);

        syn::visit::visit_item_struct(self, node);
    }

    fn visit_item_enum(&mut self, node: &'ast ItemEnum) {
        let name = node.ident.to_string();

        let mut unit = SemanticUnit::new(
            self.make_id("enum", &name),
            name,
            UnitKind::Enum,
            self.module.clone(),
            self.path.clone(),
        );

        unit.documentation = Self::documentation(&node.attrs);

        for variant in &node.variants {
            unit.fields.push(FieldInfo {
                name: variant.ident.to_string(),
                ty: format!("{:?}", variant.fields),
            });
        }

        self.finalize(&mut unit);
        self.units.push(unit);

        syn::visit::visit_item_enum(self, node);
    }

    fn visit_item_trait(&mut self, node: &'ast ItemTrait) {
        let name = node.ident.to_string();

        let mut unit = SemanticUnit::new(
            self.make_id("trait", &name),
            name,
            UnitKind::Trait,
            self.module.clone(),
            self.path.clone(),
        );

        unit.documentation = Self::documentation(&node.attrs);

        for item in &node.items {
            if let syn::TraitItem::Fn(function) = item {
                unit.references.push(function.sig.ident.to_string());
            }
        }

        self.finalize(&mut unit);
        self.units.push(unit);

        syn::visit::visit_item_trait(self, node);
    }

    fn visit_item_impl(&mut self, node: &'ast ItemImpl) {
        let target = type_string(&node.self_ty);

        let id = self.make_id("impl", &target);

        let mut unit = SemanticUnit::new(
            id,
            target.clone(),
            UnitKind::Impl,
            self.module.clone(),
            self.path.clone(),
        );

        if let Some((_, path, _)) = &node.trait_ {
            unit.traits.push(
                path.to_token_stream().to_string()
            );
        }

        self.finalize(&mut unit);
        self.units.push(unit);

        let previous = self.current_impl.clone();
        self.current_impl = Some(target);

        syn::visit::visit_item_impl(self, node);

        self.current_impl = previous;
    }

    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        self.visit_function(&node.attrs, &node.sig, &node.block);
        syn::visit::visit_item_fn(self, node);
    }
}

impl RustVisitor {
    fn visit_function(
        &mut self,
        attrs: &[syn::Attribute],
        sig: &syn::Signature,
        block: &syn::Block,
    ) {
        let name = sig.ident.to_string();

        let kind = if self.current_impl.is_some() {
            UnitKind::Method
        } else {
            UnitKind::Function
        };

        let prefix = if let Some(owner) = &self.current_impl {
            format!("method::{owner}")
        } else {
            "function".to_string()
        };

        let id = format!(
            "{}::{}::{}",
            prefix,
            self.module,
            name
        );

        let mut unit = SemanticUnit::new(
            id,
            name,
            kind,
            self.module.clone(),
            self.path.clone(),
        );

        unit.documentation = Self::documentation(attrs);
        unit.signature = sig.to_token_stream().to_string();

        for input in &sig.inputs {
            match input {
                syn::FnArg::Receiver(receiver) => {
                    unit.parameters.push(Parameter {
                        name: "self".into(),
                        ty: receiver.to_token_stream().to_string(),
                    });
                }

                syn::FnArg::Typed(argument) => {
                    let name = pattern_string(&argument.pat);

                    unit.parameters.push(Parameter {
                        name,
                        ty: type_string(&argument.ty),
                    });
                }
            }
        }

        unit.return_type = match &sig.output {
            ReturnType::Default => None,
            ReturnType::Type(_, ty) => Some(type_string(ty)),
        };

        let mut call_visitor = CallVisitor::default();
        call_visitor.visit_block(block);

        unit.calls = call_visitor.calls;
        unit.references = call_visitor.references;

        self.finalize(&mut unit);
        self.units.push(unit);
    }
}

#[derive(Default)]
struct CallVisitor {
    calls: Vec<String>,
    references: Vec<String>,
}

impl<'ast> Visit<'ast> for CallVisitor {
    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        let expression = node.func.to_token_stream().to_string();
        self.calls.push(expression);
        syn::visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        self.calls.push(node.method.to_string());
        syn::visit::visit_expr_method_call(self, node);
    }

    fn visit_path(&mut self, node: &'ast syn::Path) {
        self.references
            .push(node.to_token_stream().to_string());

        syn::visit::visit_path(self, node);
    }
}

fn parse_rust_file(path: &Path) -> io::Result<Vec<SemanticUnit>> {
    let source = fs::read_to_string(path)?;

    let syntax = syn::parse_file(&source)
        .map_err(|e| io::Error::other(format!(
            "{}: {}",
            path.display(),
            e
        )))?;

    let module = module_from_path(path);

    let mut visitor = RustVisitor::new(
        path.display().to_string(),
        module,
        source,
    );

    visitor.visit_file(&syntax);

    Ok(visitor.units)
}

fn module_from_path(path: &Path) -> String {
    let mut parts = Vec::new();

    for component in path.components() {
        let s = component.as_os_str().to_string_lossy();

        if s == "src" || s == "." {
            continue;
        }

        let s = s
            .strip_suffix(".rs")
            .unwrap_or(&s);

        if s == "lib" || s == "main" || s == "mod" {
            continue;
        }

        parts.push(s.to_string());
    }

    parts.join("::")
}

fn type_string(ty: &Type) -> String {
    ty.to_token_stream().to_string()
}

fn pattern_string(pat: &Pat) -> String {
    pat.to_token_stream().to_string()
}

// ============================================================
// REPRESENTATION
// ============================================================

fn canonical_representation(unit: &SemanticUnit) -> String {
    let mut out = String::new();

    out.push_str(match unit.kind {
        UnitKind::Module => "MODULE\n",
        UnitKind::Struct => "STRUCT\n",
        UnitKind::Enum => "ENUM\n",
        UnitKind::Trait => "TRAIT\n",
        UnitKind::Impl => "IMPL\n",
        UnitKind::Function => "FUNCTION\n",
        UnitKind::Method => "METHOD\n",
    });

    out.push_str(&format!("name={}\n", unit.name));
    out.push_str(&format!("module={}\n", unit.module));

    if !unit.parameters.is_empty() {
        out.push_str("inputs=");
        for p in &unit.parameters {
            out.push_str(&p.ty);
            out.push(';');
        }
        out.push('\n');
    }

    if let Some(ret) = &unit.return_type {
        out.push_str(&format!("output={ret}\n"));
    }

    for field in &unit.fields {
        out.push_str(&format!(
            "field={}{}\n",
            field.name,
            field.ty
        ));
    }

    for call in &unit.calls {
        out.push_str(&format!("calls={call}\n"));
    }

    for reference in &unit.references {
        out.push_str(&format!("references={reference}\n"));
    }

    for tr in &unit.traits {
        out.push_str(&format!("implements={tr}\n"));
    }

    if !unit.documentation.is_empty() {
        out.push_str("documentation=");
        out.push_str(&normalize_text(&unit.documentation));
        out.push('\n');
    }

    out
}

// ============================================================
// VECTOR / BI-ENCODER
// ============================================================

const EMBEDDING_DIM: usize = 256;

fn encode_text(text: &str) -> Vec<f32> {
    let mut vector = vec![0.0f32; EMBEDDING_DIM];

    let normalized = normalize_text(text);

    let tokens: Vec<&str> = normalized
        .split_whitespace()
        .collect();

    for token in tokens {
        let h = stable_hash(token.as_bytes());

        for i in 0..4 {
            let index = ((h.wrapping_add(i as u64 * 0x9e3779b9))
                as usize)
                % EMBEDDING_DIM;

            let sign = if ((h >> i) & 1) == 0 {
                1.0
            } else {
                -1.0
            };

            vector[index] += sign;
        }
    }

    normalize_vector(&vector)
}

fn encode_code(representation: &str) -> Vec<f32> {
    encode_text(representation)
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| x * y)
        .sum()
}

fn norm(v: &[f32]) -> f32 {
    dot(v, v).sqrt()
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let na = norm(a);
    let nb = norm(b);

    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }

    dot(a, b) / (na * nb)
}

fn euclidean_distance(a: &[f32], b: &[f32]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| {
            let d = x - y;
            d * d
        })
        .sum::<f32>()
        .sqrt()
}

fn normalize_vector(v: &[f32]) -> Vec<f32> {
    let n = norm(v);

    if n == 0.0 {
        return v.to_vec();
    }

    v.iter().map(|x| x / n).collect()
}

fn stable_hash(data: &[u8]) -> u64 {
    let digest = Sha256::digest(data);

    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&digest[..8]);

    u64::from_le_bytes(bytes)
}

// ============================================================
// REPOSITORY GRAPH
// ============================================================

fn build_graph(units: &[SemanticUnit]) -> SemanticGraph {
    let mut graph = SemanticGraph::default();

    for unit in units {
        graph.add_node(unit.clone());
    }

    let ids: Vec<String> =
        graph.nodes.keys().cloned().collect();

    for unit in units {
        for call in &unit.calls {
            if let Some(target) = resolve_symbol(call, &ids) {
                graph.add_edge(
                    unit.id.clone(),
                    target,
                    EdgeKind::Calls,
                );
            }
        }

        for reference in &unit.references {
            if let Some(target) = resolve_symbol(reference, &ids) {
                graph.add_edge(
                    unit.id.clone(),
                    target,
                    EdgeKind::References,
                );
            }
        }

        for tr in &unit.traits {
            if let Some(target) = resolve_symbol(tr, &ids) {
                graph.add_edge(
                    unit.id.clone(),
                    target,
                    EdgeKind::Implements,
                );
            }
        }
    }

    graph
}

fn resolve_symbol(reference: &str, ids: &[String]) -> Option<String> {
    let clean = reference
        .replace(' ', "")
        .replace('&', "")
        .replace("mut", "");

    ids.iter()
        .find(|id| {
            id.ends_with(&format!("::{clean}"))
                || id.contains(&format!("::{clean}::"))
                || id == &&clean
        })
        .cloned()
}

// ============================================================
// REPOSITORY SCANNER
// ============================================================

fn scan_repository(root: &Path) -> io::Result<Vec<SemanticUnit>> {
    let mut units = Vec::new();

    for entry in WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();

        if !path.is_file() {
            continue;
        }

        if path.components().any(|c| {
            let s = c.as_os_str().to_string_lossy();
            s == ".git" || s == "target"
        }) {
            continue;
        }

        if path.extension().and_then(|x| x.to_str()) != Some("rs") {
            continue;
        }

        match parse_rust_file(path) {
            Ok(mut parsed) => units.append(&mut parsed),

            Err(error) => {
                eprintln!(
                    "parser warning: {}",
                    error
                );
            }
        }
    }

    Ok(units)
}

// ============================================================
// CONTRACT EXTRACTION
// ============================================================

fn extract_contracts(root: &Path) -> io::Result<Vec<Contract>> {
    let mut contracts = Vec::new();

    let candidates = [
        "README.md",
        "README",
        "ARCHITECTURE.md",
        "CONTRACTS.md",
    ];

    for filename in candidates {
        let path = root.join(filename);

        if !path.exists() {
            continue;
        }

        let text = fs::read_to_string(path)?;

        contracts.extend(parse_contract_text(&text));
    }

    Ok(contracts)
}

fn parse_contract_text(text: &str) -> Vec<Contract> {
    let mut contracts = Vec::new();

    for line in text.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            continue;
        }

        let lower = trimmed.to_lowercase();

        let category =
            if lower.contains("must") {
                "requirement"
            } else if lower.contains("shall") {
                "contract"
            } else if lower.contains("invariant") {
                "invariant"
            } else if lower.contains("return") {
                "output"
            } else {
                continue
            };

        contracts.push(Contract {
            subject: "repository".into(),
            statement: normalize_text(trimmed),
            category: category.into(),
        });
    }

    contracts
}

// ============================================================
// SNAPSHOT BUILDER
// ============================================================

fn create_snapshot(
    root: &Path,
    commit: &str,
) -> io::Result<SemanticSnapshot> {
    let units = scan_repository(root)?;

    let graph = build_graph(&units);

    let contracts = extract_contracts(root)?;

    let contract_text = contracts
        .iter()
        .map(|c| c.statement.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    let repository_embedding =
        encode_text(&contract_text);

    let repository = root
        .file_name()
        .map(|x| x.to_string_lossy().to_string())
        .unwrap_or_else(|| "repository".into());

    let units_map =
        units.into_iter()
            .map(|u| (u.id.clone(), u))
            .collect();

    Ok(SemanticSnapshot {
        commit: commit.into(),
        repository,
        units: units_map,
        graph,
        contracts,
        repository_embedding,
    })
}

// ============================================================
// GIT
// ============================================================

fn git(root: &Path, args: &[&str]) -> io::Result<String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()?;

    if !output.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).to_string()
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_string())
}

fn current_commit(root: &Path) -> io::Result<String> {
    git(root, &["rev-parse", "HEAD"])
}

fn git_diff(
    root: &Path,
    old: &str,
    new: &str,
) -> io::Result<String> {
    git(root, &["diff", "--no-ext-diff", old, new])
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitChange {
    pub path: String,
    pub additions: usize,
    pub deletions: usize,
}

fn parse_git_diff(diff: &str) -> Vec<GitChange> {
    let mut changes = Vec::new();
    let mut current: Option<GitChange> = None;

    for line in diff.lines() {
        if let Some(path) = line.strip_prefix("+++ b/") {
            if let Some(change) = current.take() {
                changes.push(change);
            }

            current = Some(GitChange {
                path: path.into(),
                additions: 0,
                deletions: 0,
            });

            continue;
        }

        if let Some(change) = current.as_mut() {
            if line.starts_with('+') &&
                !line.starts_with("+++")
            {
                change.additions += 1;
            }

            if line.starts_with('-') &&
                !line.starts_with("---")
            {
                change.deletions += 1;
            }
        }
    }

    if let Some(change) = current {
        changes.push(change);
    }

    changes
}

// ============================================================
// SNAPSHOT COMPARISON
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChangeType {
    Added,
    Removed,
    Modified,
    Unchanged,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticChange {
    pub unit_id: String,
    pub change_type: ChangeType,
    pub previous_representation: Option<String>,
    pub current_representation: Option<String>,
    pub embedding_similarity: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticDiff {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub modified: Vec<SemanticChange>,
    pub affected: Vec<String>,
}

fn compare_snapshots(
    old: &SemanticSnapshot,
    new: &SemanticSnapshot,
) -> SemanticDiff {
    let old_ids: BTreeSet<_> =
        old.units.keys().cloned().collect();

    let new_ids: BTreeSet<_> =
        new.units.keys().cloned().collect();

    let added = new_ids
        .difference(&old_ids)
        .cloned()
        .collect::<Vec<_>>();

    let removed = old_ids
        .difference(&new_ids)
        .cloned()
        .collect::<Vec<_>>();

    let mut modified = Vec::new();

    for id in old_ids.intersection(&new_ids) {
        let previous = &old.units[id];
        let current = &new.units[id];

        if previous.representation != current.representation {
            let similarity =
                cosine_similarity(
                    &previous.embedding,
                    &current.embedding,
                );

            modified.push(SemanticChange {
                unit_id: id.clone(),
                change_type: ChangeType::Modified,
                previous_representation:
                    Some(previous.representation.clone()),
                current_representation:
                    Some(current.representation.clone()),
                embedding_similarity: similarity,
            });
        }
    }

    let mut roots = Vec::new();

    roots.extend(added.iter().cloned());
    roots.extend(removed.iter().cloned());
    roots.extend(
        modified.iter().map(|x| x.unit_id.clone())
    );

    let affected_set =
        new.graph.affected_units(&roots);

    let affected =
        affected_set.into_iter().collect();

    SemanticDiff {
        added,
        removed,
        modified,
        affected,
    }
}

// ============================================================
// DSPY BRIDGE
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DspyRequest {
    pub previous: Vec<String>,
    pub current: Vec<String>,
    pub changed_units: Vec<String>,
    pub contracts: Vec<String>,
    pub embedding_similarity: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DspyAnalysis {
    pub preserved_intent: Vec<String>,
    pub changed_intent: Vec<String>,
    pub broken_contracts: Vec<String>,
    pub introduced_behavior: Vec<String>,
    pub removed_behavior: Vec<String>,
    pub continuity_assessment: f32,
    pub explanation: String,
}

pub struct DspyClient {
    endpoint: String,
}

impl DspyClient {
    pub fn new(endpoint: String) -> Self {
        Self { endpoint }
    }

    pub fn analyze(
        &self,
        request: &DspyRequest,
    ) -> Result<DspyAnalysis, String> {
        let client =
            reqwest::blocking::Client::new();

        let response = client
            .post(&self.endpoint)
            .json(request)
            .send()
            .map_err(|e| e.to_string())?;

        if !response.status().is_success() {
            return Err(format!(
                "DSPy returned HTTP {}",
                response.status()
            ));
        }

        response
            .json::<DspyAnalysis>()
            .map_err(|e| e.to_string())
    }
}

// ============================================================
// CONTINUITY ENGINE
// ============================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GateStatus {
    Pass,
    Panic,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub category: String,
    pub subject: String,
    pub detail: String,
    pub severity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContinuityResult {
    pub status: GateStatus,
    pub continuity: f32,
    pub semantic_changes: Vec<SemanticChange>,
    pub broken_contracts: Vec<String>,
    pub evidence: Vec<Evidence>,
    pub explanation: String,
}

pub struct ContinuityEngine {
    pub panic_threshold: f32,
    pub contract_threshold: f32,
}

impl Default for ContinuityEngine {
    fn default() -> Self {
        Self {
            panic_threshold: 0.60,
            contract_threshold: 0.70,
        }
    }
}

impl ContinuityEngine {
    pub fn evaluate(
        &self,
        old: &SemanticSnapshot,
        new: &SemanticSnapshot,
        diff: &SemanticDiff,
        dspy: Option<&DspyAnalysis>,
    ) -> ContinuityResult {
        let mut evidence = Vec::new();
        let mut scores = Vec::new();

        for change in &diff.modified {
            scores.push(change.embedding_similarity);

            if change.embedding_similarity < self.panic_threshold {
                evidence.push(Evidence {
                    category: "semantic_drift".into(),
                    subject: change.unit_id.clone(),
                    detail: format!(
                        "embedding similarity {:.3} is below threshold {:.3}",
                        change.embedding_similarity,
                        self.panic_threshold
                    ),
                    severity: "panic".into(),
                });
            }
        }

        for added in &diff.added {
            evidence.push(Evidence {
                category: "added_unit".into(),
                subject: added.clone(),
                detail: "new semantic unit introduced".into(),
                severity: "info".into(),
            });
        }

        for removed in &diff.removed {
            evidence.push(Evidence {
                category: "removed_unit".into(),
                subject: removed.clone(),
                detail: "semantic unit removed".into(),
                severity: "panic".into(),
            });
        }

        let contract_score =
            contract_similarity(
                &old.contracts,
                &new.contracts,
            );

        if contract_score < self.contract_threshold {
            evidence.push(Evidence {
                category: "contract_drift".into(),
                subject: "repository".into(),
                detail: format!(
                    "contract similarity {:.3}",
                    contract_score
                ),
                severity: "panic".into(),
            });
        }

        scores.push(contract_score);

        if let Some(analysis) = dspy {
            if analysis.continuity_assessment
                < self.panic_threshold
            {
                evidence.push(Evidence {
                    category: "dspy_semantic_analysis".into(),
                    subject: "repository".into(),
                    detail: analysis.explanation.clone(),
                    severity: "panic".into(),
                });
            }

            scores.push(
                analysis.continuity_assessment
            );

            for contract in &analysis.broken_contracts {
                evidence.push(Evidence {
                    category: "broken_contract".into(),
                    subject: "repository".into(),
                    detail: contract.clone(),
                    severity: "panic".into(),
                });
            }
        }

        let continuity =
            if scores.is_empty() {
                1.0
            } else {
                scores.iter().sum::<f32>()
                    / scores.len() as f32
            };

        let has_panic =
            evidence.iter().any(|e| {
                e.severity == "panic"
            });

        let status = if has_panic ||
            continuity < self.panic_threshold
        {
            GateStatus::Panic
        } else {
            GateStatus::Pass
        };

        let explanation =
            build_explanation(
                &status,
                continuity,
                &evidence,
            );

        ContinuityResult {
            status,
            continuity,
            semantic_changes: diff.modified.clone(),
            broken_contracts: evidence
                .iter()
                .filter(|e| e.category == "broken_contract")
                .map(|e| e.detail.clone())
                .collect(),
            evidence,
            explanation,
        }
    }
}

fn contract_similarity(
    old: &[Contract],
    new: &[Contract],
) -> f32 {
    if old.is_empty() && new.is_empty() {
        return 1.0;
    }

    let old_text = old
        .iter()
        .map(|x| x.statement.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    let new_text = new
        .iter()
        .map(|x| x.statement.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    cosine_similarity(
        &encode_text(&old_text),
        &encode_text(&new_text),
    )
}

fn build_explanation(
    status: &GateStatus,
    continuity: f32,
    evidence: &[Evidence],
) -> String {
    match status {
        GateStatus::Pass => format!(
            "semantic continuity preserved at {:.3}; {} evidence items",
            continuity,
            evidence.len()
        ),

        GateStatus::Panic => {
            let severe = evidence
                .iter()
                .filter(|x| x.severity == "panic")
                .count();

            format!(
                "semantic continuity failed at {:.3}; {} blocking evidence items",
                continuity,
                severe
            )
        }
    }
}

// ============================================================
// NORMALIZATION
// ============================================================

fn normalize_text(text: &str) -> String {
    text.split_whitespace()
        .map(|x| {
            x.trim_matches(|c: char| {
                !c.is_alphanumeric() &&
                c != '_' &&
                c != ':' &&
                c != '&' &&
                c != '<' &&
                c != '>' &&
                c != '-' &&
                c != '/'
            })
        })
        .filter(|x| !x.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

// ============================================================
// REPORTING
// ============================================================

fn print_result(result: &ContinuityResult) {
    println!();
    println!("PANIC ROOM");
    println!("────────────────────────────────────────");
    println!(
        "Continuity: {:.3}",
        result.continuity
    );

    match result.status {
        GateStatus::Pass => {
            println!("Status: PASS");
        }

        GateStatus::Panic => {
            println!("Status: PANIC");
        }
    }

    println!();

    for evidence in &result.evidence {
        println!(
            "[{}] {} :: {}",
            evidence.severity.to_uppercase(),
            evidence.category,
            evidence.subject
        );

        println!(
            "    {}",
            evidence.detail
        );
    }

    println!();
    println!("{}", result.explanation);
    println!();
}

fn print_json(result: &ContinuityResult) {
    match serde_json::to_string_pretty(result) {
        Ok(json) => println!("{json}"),
        Err(error) => eprintln!(
            "serialization error: {error}"
        ),
    }
}

// ============================================================
// COMMANDS
// ============================================================

fn command_snapshot(root: &Path) -> io::Result<()> {
    let commit = current_commit(root)?;

    println!(
        "creating semantic snapshot {}",
        commit
    );

    let snapshot =
        create_snapshot(root, &commit)?;

    snapshot.save(root)?;

    println!(
        "snapshot saved: {} units",
        snapshot.units.len()
    );

    Ok(())
}

fn command_check(
    root: &Path,
    old_commit: &str,
    new_commit: &str,
) -> io::Result<bool> {
    let old =
        SemanticSnapshot::load(root, old_commit)?;

    let new =
        SemanticSnapshot::load(root, new_commit)
            .or_else(|_| {
                create_snapshot(
                    root,
                    new_commit,
                )
            })?;

    let diff =
        compare_snapshots(&old, &new);

    let git_diff =
        git_diff(
            root,
            old_commit,
            new_commit,
        )?;

    let changes =
        parse_git_diff(&git_diff);

    println!(
        "Git changes: {} files",
        changes.len()
    );

    println!(
        "Semantic changes: {}",
        diff.modified.len()
    );

    println!(
        "Affected units: {}",
        diff.affected.len()
    );

    let dspy = env::var(
        "PANIC_ROOM_DSPY_URL"
    )
    .ok()
    .and_then(|endpoint| {
        let previous = diff.modified
            .iter()
            .filter_map(|x| {
                x.previous_representation.clone()
            })
            .collect();

        let current = diff.modified
            .iter()
            .filter_map(|x| {
                x.current_representation.clone()
            })
            .collect();

        let changed_units =
            diff.affected.clone();

        let contracts = new.contracts
            .iter()
            .map(|x| x.statement.clone())
            .collect();

        let similarities =
            diff.modified
                .iter()
                .map(|x| x.embedding_similarity)
                .collect::<Vec<_>>();

        let embedding_similarity =
            if similarities.is_empty() {
                1.0
            } else {
                similarities.iter().sum::<f32>()
                    / similarities.len() as f32
            };

        let request = DspyRequest {
            previous,
            current,
            changed_units,
            contracts,
            embedding_similarity,
        };

        match DspyClient::new(endpoint)
            .analyze(&request)
        {
            Ok(result) => Some(result),

            Err(error) => {
                eprintln!(
                    "DSPy unavailable: {error}"
                );
                None
            }
        }
    });

    let engine =
        ContinuityEngine::default();

    let result =
        engine.evaluate(
            &old,
            &new,
            &diff,
            dspy.as_ref(),
        );

    print_result(&result);

    Ok(matches!(
        result.status,
        GateStatus::Pass
    ))
}

fn command_diff(
    root: &Path,
    old: &str,
    new: &str,
) -> io::Result<()> {
    let diff =
        git_diff(root, old, new)?;

    for change in parse_git_diff(&diff) {
        println!(
            "{} +{} -{}",
            change.path,
            change.additions,
            change.deletions
        );
    }

    Ok(())
}

fn command_status(root: &Path) -> io::Result<()> {
    let commit =
        current_commit(root)?;

    println!(
        "HEAD: {}",
        commit
    );

    let snapshot_dir =
        root.join(".panic-room")
            .join("snapshots");

    if !snapshot_dir.exists() {
        println!("snapshot: none");
        return Ok(());
    }

    let count =
        fs::read_dir(snapshot_dir)?
            .filter_map(Result::ok)
            .count();

    println!(
        "snapshots: {}",
        count
    );

    Ok(())
}

fn command_install(root: &Path) -> io::Result<()> {
    let git_dir = root.join(".git");

    if !git_dir.exists() {
        return Err(io::Error::other(
            "not a Git repository"
        ));
    }

    let hooks = git_dir.join("hooks");

    fs::create_dir_all(&hooks)?;

    let hook = hooks.join("pre-push");

    let executable = r#"#!/bin/sh
set -eu

REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

panic-room snapshot

OLD="$(git rev-parse HEAD~1 2>/dev/null || true)"
NEW="$(git rev-parse HEAD)"

if [ -n "$OLD" ]; then
    panic-room check "$OLD" "$NEW"
fi
"#;

    fs::write(&hook, executable)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions =
            fs::metadata(&hook)?.permissions();

        permissions.set_mode(0o755);

        fs::set_permissions(
            &hook,
            permissions,
        )?;
    }

    println!(
        "installed {}",
        hook.display()
    );

    Ok(())
}

// ============================================================
// CLI
// ============================================================

fn usage() {
    println!(
r#"panic-room

Commands:

    panic-room snapshot
    panic-room check <OLD> <NEW>
    panic-room diff <OLD> <NEW>
    panic-room status
    panic-room install

Environment:

    PANIC_ROOM_DSPY_URL
        Optional DSPy semantic-analysis endpoint.

Examples:

    panic-room snapshot

    panic-room check HEAD~1 HEAD

    panic-room diff HEAD~1 HEAD

    panic-room install
"#
    );
}

fn root_from_git() -> io::Result<PathBuf> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()?;

    if !output.status.success() {
        return Err(io::Error::other(
            "not inside a Git repository"
        ));
    }

    Ok(PathBuf::from(
        String::from_utf8_lossy(
            &output.stdout
        ).trim()
    ))
}

fn main() -> ExitCode {
    let args: Vec<String> =
        env::args().collect();

    if args.len() < 2 {
        usage();
        return ExitCode::from(2);
    }

    let root =
        match root_from_git() {
            Ok(root) => root,

            Err(error) => {
                eprintln!("{error}");
                return ExitCode::from(2);
            }
        };

    let result = match args[1].as_str() {
        "snapshot" => {
            command_snapshot(&root)
                .map(|_| true)
        }

        "check" => {
            if args.len() != 4 {
                usage();
                return ExitCode::from(2);
            }

            command_check(
                &root,
                &args[2],
                &args[3],
            )
        }

        "diff" => {
            if args.len() != 4 {
                usage();
                return ExitCode::from(2);
            }

            command_diff(
                &root,
                &args[2],
                &args[3],
            )
            .map(|_| true)
        }

        "status" => {
            command_status(&root)
                .map(|_| true)
        }

        "install" => {
            command_install(&root)
                .map(|_| true)
        }

        "json-check" => {
            if args.len() != 4 {
                usage();
                return ExitCode::from(2);
            }

            match (
                SemanticSnapshot::load(
                    &root,
                    &args[2],
                ),
                SemanticSnapshot::load(
                    &root,
                    &args[3],
                ),
            ) {
                (Ok(old), Ok(new)) => {
                    let diff =
                        compare_snapshots(
                            &old,
                            &new,
                        );

                    let engine =
                        ContinuityEngine::default();

                    let result =
                        engine.evaluate(
                            &old,
                            &new,
                            &diff,
                            None,
                        );

                    print_json(&result);

                    Ok(matches!(
                        result.status,
                        GateStatus::Pass
                    ))
                }

                (Err(a), Err(b)) => {
                    Err(io::Error::other(
                        format!(
                            "unable to load snapshots: {a}; {b}"
                        )
                    ))
                }

                (Err(error), _) |
                (_, Err(error)) => {
                    Err(error)
                }
            }
        }

        _ => {
            usage();
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(true) => ExitCode::SUCCESS,

        Ok(false) => ExitCode::from(1),

        Err(error) => {
            eprintln!(
                "panic-room error: {error}"
            );

            ExitCode::from(2)
        }
    }
}

// ============================================================
// sha256 helper (project-internal, no external dep needed)
// ============================================================

fn sha256(data: &str) -> String {
    let digest = Sha256::digest(data.as_bytes());
    format!("{digest:x}")
}