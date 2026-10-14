//! Host-owned, serializable description of the existing text behavior chain.
//! This first batch validates a graph; the built-in executor delegates to the
//! unchanged Engine::cycle. No graph node calls a model or mutates the Store.
use super::{CycleStart, Engine};
use anyhow::Result;
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const GRAPH_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub three_layer: bool,
    pub sending: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeClass { Core, Behavior }

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect { HostAtomic, NoLockAsync, Pure }

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Parameter {
    pub name: String,
    pub schema: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub name: String,
    pub class: NodeClass,
    pub slot: Option<String>,
    pub port_type: Option<String>,
    pub parameters: Vec<Parameter>,
    pub consumes: Vec<String>,
    pub produces: Vec<String>,
    pub dependencies: Vec<String>,
    pub effect: Effect,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind { Fixed, Data, Extension }

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct Edge {
    pub from: String,
    pub to: String,
    pub artifact: Option<String>,
    pub kind: EdgeKind,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Route {
    pub trigger: String,
    pub gate: String,
    pub executor: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GraphDocument {
    pub schema_version: u16,
    pub graph_hash: String,
    pub profile: Profile,
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub order: Vec<String>,
    pub routes: Vec<Route>,
    pub switches: Vec<SwitchState>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SwitchState {
    pub component: String,
    pub requested_enabled: bool,
    pub effective: bool,
    pub reason: String,
}

pub type CompiledGraph = GraphDocument;

/// Observations from the unchanged built-in executor. A trace is diagnostic:
/// it never authorizes a transition or substitutes for a graph node executor.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodeVisit {
    pub node: String,
    pub outcome: String,
}

pub(super) struct CycleTrace<'a> {
    engine: &'a Engine,
    chat: &'a str,
    visits: Vec<NodeVisit>,
    terminal: Option<String>,
}

impl<'a> CycleTrace<'a> {
    pub(super) fn new(engine: &'a Engine, chat: &'a str) -> Self {
        Self { engine, chat, visits: Vec::new(), terminal: None }
    }

    pub(super) fn visit(&mut self, node: &str, outcome: &str) {
        if self.engine.behavior_graph.is_some() {
            self.visits.push(NodeVisit { node: node.into(), outcome: outcome.into() });
        }
    }

    pub(super) fn stop(&mut self, reason: &str) {
        self.terminal = Some(reason.into());
    }

    pub(super) fn is_stopped(&self) -> bool {
        self.terminal.is_some()
    }
}

impl Drop for CycleTrace<'_> {
    fn drop(&mut self) {
        if let Some(graph) = &self.engine.behavior_graph {
            (self.engine.options.log)("behavior_path", json!({
                "chat": self.chat,
                "graph_hash": graph.graph_hash,
                "visits": self.visits,
                "terminal": self.terminal.as_deref().unwrap_or("unclassified"),
            }));
        }
    }
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum GraphError {
    #[error("duplicate_node:{0}")]
    DuplicateNode(String),
    #[error("unknown_node:{0}")]
    UnknownNode(String),
    #[error("fixed_node_override:{0}")]
    FixedNodeOverride(String),
    #[error("cycle")]
    Cycle,
    #[error("missing_producer:{node}:{artifact}")]
    MissingProducer { node: String, artifact: String },
    #[error("type_mismatch:{from}->{to}:{artifact}")]
    TypeMismatch { from: String, to: String, artifact: String },
    #[error("missing_dependency:{node}:{dependency}")]
    MissingDependency { node: String, dependency: String },
    #[error("exclusive_producer:{artifact}")]
    ExclusiveProducer { artifact: String },
    #[error("unreachable:{0}")]
    Unreachable(String),
    #[error("safety_bypass:{0}")]
    SafetyBypass(String),
    #[error("illegal_insertion:{0}")]
    IllegalInsertion(String),
    #[error("unknown_artifact_schema:{0}")]
    UnknownArtifactSchema(String),
}

/// JSON schema is host-owned. The first batch has no external behavior runner;
/// future responses must be checked against these schemas, never self-reported ones.
pub fn artifact_schema(id: &str) -> Option<Value> {
    let (kind, required): (&str, &[&str]) = match id {
        "raw_event" => ("object", &["event"]),
        "authorized_message" => ("object", &["chat", "sender", "ts", "received_at"]),
        "unique_message" => ("object", &["message_key"]),
        "evidence_history" | "prompt_history" | "candidates" | "ratings" | "segments" => ("array", &[]),
        "articulated_draft" => ("object", &["text"]),
        "validated_output" => ("object", &["segments"]),
        "turn_token" => ("object", &["chat", "version"]),
        "delivery" => ("object", &["delivery_id"]),
        "ack" => ("object", &["status"]),
        "outbound" => ("object", &["message_key"]),
        "normalized" | "pending_turn" | "handled_turn" | "trigger" | "fresh_turn" | "orientation_ready"
        | "admitted_turn" | "valid_candidates" | "learned_candidates"
        | "committed_candidates" | "valid_ratings" | "selected" | "forecast"
        | "assessment" | "no_assessment" | "repetition_result" | "topic_result"
        | "targeting_result" | "final_permit" | "delayed_delivery"
        | "segment_permit" | "classified_ack" | "untrusted_items" | "audited_items"
        | "external_context" => ("object", &[]),
        _ => return None,
    };
    Some(json!({"$schema":"https://json-schema.org/draft/2020-12/schema",
        "$id":format!("qq-inner/behavior/v1/{id}"),"type":kind,"required":required}))
}

pub fn validate_artifact(id: &str, value: &Value) -> bool {
    let Some(schema) = artifact_schema(id) else { return false };
    let type_ok = match schema["type"].as_str() {
        Some("array") => value.is_array(),
        Some("object") => value.is_object(),
        _ => false,
    };
    type_ok && schema["required"].as_array().is_some_and(|keys| keys.iter().all(|key| {
        key.as_str().is_some_and(|name| value.get(name).is_some_and(|v| !v.is_null()))
    }))
}

fn node(id: &str, name: &str, consumes: &[&str], produces: &[&str]) -> Node {
    let class = if id.starts_with("K.") { NodeClass::Core } else { NodeClass::Behavior };
    let port_type = match id {
        "B.formation" | "B.evaluation" | "B.forecast" | "B.articulation" => Some("ModelPort".into()),
        "B.external_fetch" => Some("ExternalPort".into()),
        "K.deliver" => Some("ChatPort".into()),
        _ => None,
    };
    let parameters = match id {
        "K.reserve_chat" => vec![Parameter{name:"max_concurrent_chats".into(),schema:"number".into()}],
        "B.backlog_digest" => vec![Parameter{name:"enabled".into(),schema:"boolean".into()}],
        "K.validate_final_output" => vec![Parameter{name:"max_output_chars".into(),schema:"integer".into()}],
        _ => vec![],
    };
    Node { id:id.into(), name:name.into(), class:class.clone(),
        slot:(class == NodeClass::Behavior).then(|| id.trim_start_matches("B.").to_owned()),
        port_type, parameters, consumes:consumes.iter().map(|s|s.to_string()).collect(),
        produces:produces.iter().map(|s|s.to_string()).collect(), dependencies:vec![],
        effect:if class == NodeClass::Core { Effect::HostAtomic } else { Effect::NoLockAsync } }
}

fn edge(from: &str, to: &str, artifact: Option<&str>, kind: EdgeKind) -> Edge {
    Edge { from:from.into(), to:to.into(), artifact:artifact.map(str::to_owned), kind }
}

fn fixed_graph(profile: Profile) -> (Vec<Node>, Vec<Edge>, Vec<Route>) {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    // Each tuple is an existing semantic stage, not a new executor. The artifact
    // on the edge is the output consumed by the following stage.
    let prefix = [
        ("K.normalize","Normalize input","raw_event","normalized"),
        ("K.authorize","Authorize chat and account","normalized","authorized_message"),
        ("K.deduplicate","Record unique message","authorized_message","unique_message"),
        ("B.capture","Capture perception","unique_message","evidence_history"),
        ("B.pending","Mark live pending turn","evidence_history","pending_turn"),
        ("K.reserve_chat","Reserve chat and concurrency slot","pending_turn","turn_token"),
    ];
    add_chain(&prefix, &mut nodes, &mut edges);
    // Eligibility is independent. The host route retains reply priority.
    let gates: &[(&str,&str)] = if profile.three_layer {
        &[("B.reply_gate","message"),("B.topic_gate","topic")]
    } else {
        &[("B.legacy_reply_gate","message"),("B.pause_gate","pause")]
    };
    nodes.push(node("K.route_reply_first","Select one trigger",&[], &["trigger"]));
    for (id, _) in gates {
        let topic=id==&"B.topic_gate";
        nodes.push(node(id, "Evaluate trigger eligibility", if topic { &["handled_turn"] } else { &["turn_token"] }, &[]));
        if topic {
            edges.push(edge("K.handled_match",id,Some("handled_turn"),EdgeKind::Data));
        } else {
            edges.push(edge("K.reserve_chat", id, Some("turn_token"), EdgeKind::Data));
        }
        edges.push(edge(id,"K.route_reply_first",None,EdgeKind::Fixed));
    }
    if profile.three_layer {
        nodes.push(node("K.handled_match","Require handled message before topic", &["turn_token"], &["handled_turn"]));
        edges.push(edge("K.reserve_chat","K.handled_match",Some("turn_token"),EdgeKind::Data));
    }
    let routes = gates.iter().map(|(gate, trigger)| Route {
        trigger:(*trigger).into(), gate:(*gate).into(), executor:"Engine::cycle".into(),
    }).collect();
    let mut suffix = vec![
        ("K.revalidate_admission","Revalidate turn","trigger","fresh_turn"),
        ("B.orientation_ready","Observe group before speaking","fresh_turn","orientation_ready"),
        ("K.speak_admission","Apply speaking gate","orientation_ready","admitted_turn"),
        ("B.backlog_digest","Prepare prompt history","admitted_turn","prompt_history"),
        ("B.formation","Form candidate ideas","prompt_history","candidates"),
        ("K.validate_candidates","Validate candidates","candidates","valid_candidates"),
        ("B.learning","Review optional learning","valid_candidates","learned_candidates"),
        ("K.commit_learning","Commit fresh learning","learned_candidates","committed_candidates"),
        ("B.evaluation","Evaluate ideas","committed_candidates","ratings"),
        ("K.validate_ratings","Validate ratings","ratings","valid_ratings"),
        ("B.selection","Select one candidate","valid_ratings","selected"),
    ];
    if profile.sending {
        suffix.extend([
            ("B.forecast","Forecast response","selected","forecast"),
            ("K.assess_once","Record sending assessment once","forecast","assessment"),
            ("B.articulation","Articulate selected idea","assessment","articulated_draft"),
        ]);
    } else {
        suffix.extend([
            ("K.no_assessment_route","Select existing no-assessment route","selected","no_assessment"),
            ("B.articulation","Articulate selected idea","no_assessment","articulated_draft"),
        ]);
    }
    suffix.extend([
        ("K.canonicalize_segments","Canonicalize actual outgoing segments","articulated_draft","segments"),
        ("K.validate_final_output","Validate final outgoing content","segments","validated_output"),
        ("B.repetition","Check repetition","validated_output","repetition_result"),
        ("B.topic_lifecycle","Check topic lifecycle","repetition_result","topic_result"),
        ("B.targeting","Resolve reply target and mention","topic_result","targeting_result"),
        ("K.final_admission","Apply final sending gate","targeting_result","final_permit"),
        ("K.reserve_delivery","Write pending delivery","final_permit","delivery"),
        ("K.delay","Wait before segment","delivery","delayed_delivery"),
        ("K.revalidate_segment","Recheck segment after delay","delayed_delivery","segment_permit"),
        ("K.deliver","Deliver segment through chat transport","segment_permit","ack"),
        ("K.classify_ack","Classify delivery result","ack","classified_ack"),
        ("K.record_outbound","Record confirmed outbound evidence","classified_ack","outbound"),
    ]);
    add_chain(&suffix, &mut nodes, &mut edges);
    edges.push(edge("K.route_reply_first","K.revalidate_admission",Some("trigger"),EdgeKind::Data));
    // Source material can only enter the formation context through audit.
    for (id,name,consume,produce) in [
        ("B.external_fetch","Fetch untrusted topic data","admitted_turn","untrusted_items"),
        ("K.untrusted_audit","Audit untrusted topic data","untrusted_items","audited_items"),
        ("B.context_inject","Inject audited context","audited_items","external_context"),
    ] { nodes.push(node(id,name,&[consume],&[produce])); }
    edges.push(edge("K.speak_admission","B.external_fetch",Some("admitted_turn"),EdgeKind::Data));
    edges.push(edge("B.external_fetch","K.untrusted_audit",Some("untrusted_items"),EdgeKind::Data));
    edges.push(edge("K.untrusted_audit","B.context_inject",Some("audited_items"),EdgeKind::Data));
    edges.push(edge("B.context_inject","B.formation",None,EdgeKind::Fixed));
    (nodes, edges, routes)
}

fn add_chain(stages: &[(&str,&str,&str,&str)], nodes: &mut Vec<Node>, edges: &mut Vec<Edge>) {
    for (i,(id,name,input,output)) in stages.iter().enumerate() {
        nodes.push(node(id,name,&[*input],&[*output]));
        if i > 0 { edges.push(edge(stages[i-1].0,id,Some(input),EdgeKind::Data)); }
    }
}

/// Fixed edges and core nodes are always constructed by the host. Extensions
/// may add nodes/edges only; there is no removal or override API.
pub fn compile(profile: Profile, extra_nodes: &[Node], extra_edges: &[Edge], bindings: &[String])
    -> std::result::Result<CompiledGraph, GraphError> {
    let (mut nodes, mut edges, routes) = fixed_graph(profile);
    for extension in extra_nodes {
        if extension.id.starts_with("K.") || extension.class != NodeClass::Behavior {
            return Err(GraphError::FixedNodeOverride(extension.id.clone()));
        }
        if extension.effect == Effect::HostAtomic { return Err(GraphError::IllegalInsertion(extension.id.clone())); }
        if nodes.iter().any(|n| n.id == extension.id) { return Err(GraphError::DuplicateNode(extension.id.clone())); }
        nodes.push(extension.clone());
    }
    for extension in extra_edges {
        if extension.kind != EdgeKind::Extension { return Err(GraphError::IllegalInsertion(extension.from.clone())); }
        edges.push(extension.clone());
    }
    let by_id: BTreeMap<_,_> = nodes.iter().map(|n|(n.id.as_str(),n)).collect();
    for n in &nodes {
        for artifact in n.consumes.iter().chain(&n.produces) {
            if artifact_schema(artifact).is_none() { return Err(GraphError::UnknownArtifactSchema(artifact.clone())); }
        }
        for dependency in &n.dependencies {
            if !bindings.contains(dependency) { return Err(GraphError::MissingDependency {node:n.id.clone(),dependency:dependency.clone()}); }
        }
    }
    for e in &edges {
        let from=by_id.get(e.from.as_str()).ok_or_else(||GraphError::UnknownNode(e.from.clone()))?;
        let to=by_id.get(e.to.as_str()).ok_or_else(||GraphError::UnknownNode(e.to.clone()))?;
        if let Some(a)=&e.artifact {
            if !from.produces.contains(a) || !to.consumes.contains(a) {
                return Err(GraphError::TypeMismatch {from:e.from.clone(),to:e.to.clone(),artifact:a.clone()});
            }
        }
    }
    let mut producers=BTreeMap::<&str,&str>::new();
    for n in &nodes { for artifact in &n.produces {
        if producers.insert(artifact,n.id.as_str()).is_some() { return Err(GraphError::ExclusiveProducer {artifact:artifact.clone()}); }
    }}
    for n in &nodes { for artifact in &n.consumes {
        if n.id == "K.normalize" && artifact == "raw_event" { continue; }
        if !edges.iter().any(|e|e.to==n.id && e.artifact.as_deref()==Some(artifact)) {
            return Err(GraphError::MissingProducer {node:n.id.clone(),artifact:artifact.clone()});
        }
    }}
    let order=topological(&nodes,&edges)?;
    let forward=reachable("K.normalize",None,&edges);
    let backward=reverse_reachable("K.record_outbound",&edges);
    for n in &nodes {
        if !forward.contains(&n.id) || !backward.contains(&n.id) { return Err(GraphError::Unreachable(n.id.clone())); }
    }
    let mut required_dominators=vec![
        ("K.normalize","K.authorize","K.record_outbound"),
        ("K.normalize","K.deduplicate","B.capture"),
        ("K.normalize","K.reserve_chat","K.deliver"),
        ("K.normalize","K.revalidate_admission","B.formation"),
        ("K.normalize","B.orientation_ready","K.speak_admission"),
        ("K.normalize","K.speak_admission","B.formation"),
        ("K.normalize","K.validate_candidates","B.evaluation"),
        ("K.normalize","K.validate_ratings","B.selection"),
        ("K.normalize","K.validate_final_output","K.deliver"),
        ("K.normalize","K.final_admission","K.deliver"),
        ("K.normalize","K.reserve_delivery","K.deliver"),
        ("K.normalize","K.revalidate_segment","K.deliver"),
        ("K.normalize","K.classify_ack","K.record_outbound"),
    ];
    if profile.three_layer {
        required_dominators.push(("K.reserve_chat","K.handled_match","B.topic_gate"));
    }
    if profile.sending {
        required_dominators.push(("K.normalize","K.assess_once","B.articulation"));
    }
    for (start,guard,target) in required_dominators {
        if reachable(start,Some(guard),&edges).contains(target) {
            return Err(GraphError::SafetyBypass(guard.into()));
        }
    }
    if reachable("B.external_fetch",Some("K.untrusted_audit"),&edges).contains("B.context_inject") {
        return Err(GraphError::SafetyBypass("K.untrusted_audit".into()));
    }
    for n in extra_nodes {
        if n.slot.as_deref()==Some("post_articulation_text_revision") &&
            (!reachable("B.articulation",None,&edges).contains(&n.id) ||
             !reachable(&n.id,None,&edges).contains("K.canonicalize_segments")) {
            return Err(GraphError::IllegalInsertion(n.id.clone()));
        }
    }
    nodes.sort_by(|a,b|a.id.cmp(&b.id));
    edges.sort(); edges.dedup();
    let mut switches:Vec<_>=nodes.iter().filter(|n|n.class==NodeClass::Behavior).map(|n|SwitchState{
        component:n.id.clone(),requested_enabled:true,effective:true,reason:"active".into()
    }).collect();
    for id in ["B.topic_gate","B.pause_gate","B.forecast"] {
        if !nodes.iter().any(|n|n.id==id) {switches.push(SwitchState{
            component:id.into(),requested_enabled:false,effective:false,
            reason:"disabled_by_profile".into()
        });}
    }
    switches.sort_by(|a,b|a.component.cmp(&b.component));
    let mut document=GraphDocument{schema_version:GRAPH_SCHEMA_VERSION,graph_hash:String::new(),profile,nodes,edges,order,routes,switches};
    document.graph_hash=format!("{:x}",Sha256::digest(serde_json::to_vec(&document).expect("serializable graph")));
    Ok(document)
}

fn topological(nodes:&[Node],edges:&[Edge])->std::result::Result<Vec<String>,GraphError>{
    let mut degree:BTreeMap<String,usize>=nodes.iter().map(|n|(n.id.clone(),0)).collect();
    for e in edges { *degree.get_mut(&e.to).expect("checked edge")+=1; }
    let mut ready:BTreeSet<String>=degree.iter().filter(|(_,d)|**d==0).map(|(id,_)|id.clone()).collect();
    let mut out=Vec::with_capacity(nodes.len());
    while let Some(id)=ready.pop_first(){
        out.push(id.clone());
        for e in edges.iter().filter(|e|e.from==id){
            let d=degree.get_mut(&e.to).expect("checked edge"); *d-=1;
            if *d==0 {ready.insert(e.to.clone());}
        }
    }
    if out.len()!=nodes.len(){return Err(GraphError::Cycle)}
    Ok(out)
}
fn reachable(start:&str,excluded:Option<&str>,edges:&[Edge])->BTreeSet<String>{
    let mut seen=BTreeSet::new();
    if excluded==Some(start){return seen}
    let mut pending=vec![start.to_owned()];
    while let Some(id)=pending.pop(){
        if !seen.insert(id.clone()){continue}
        for e in edges.iter().filter(|e|e.from==id && excluded!=Some(e.to.as_str())){pending.push(e.to.clone());}
    }
    seen
}
fn reverse_reachable(start:&str,edges:&[Edge])->BTreeSet<String>{
    let mut seen=BTreeSet::new(); let mut pending=vec![start.to_owned()];
    while let Some(id)=pending.pop(){
        if !seen.insert(id.clone()){continue}
        for e in edges.iter().filter(|e|e.to==id){pending.push(e.from.clone());}
    }
    seen
}

/// Batch one keeps the existing turn as a single built-in implementation.
/// Slot-level execution and foreign implementations are later batches.
pub(super) trait TextBehavior: Send + Sync {
    fn run<'a>(&'a self, engine: &'a Engine, graph: &'a CompiledGraph,
        chat: &'a str, trigger: &'a str, start: CycleStart) -> BoxFuture<'a, Result<()>>;
}
pub struct BuiltinText;
impl TextBehavior for BuiltinText {
    fn run<'a>(&'a self, engine:&'a Engine, graph:&'a CompiledGraph,
        chat:&'a str, trigger:&'a str, start:CycleStart)->BoxFuture<'a,Result<()>> {
        Box::pin(async move {
            anyhow::ensure!(graph.routes.iter().any(|r|r.trigger==trigger && r.executor=="Engine::cycle"),"behavior_route_unavailable");
            engine.cycle(chat,trigger,start).await
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn profile() -> Profile { Profile { three_layer:true, sending:true } }
    fn extra(id:&str, consumes:&[&str], produces:&[&str]) -> Node {
        node(id,id,consumes,produces)
    }
    #[test]
    fn default_graph_round_trip_and_metadata() {
        let graph=compile(profile(),&[],&[],&[]).unwrap();
        assert_eq!(graph.order.len(),graph.nodes.len());
        assert_eq!(graph.routes.iter().map(|r|r.trigger.as_str()).collect::<Vec<_>>(),vec!["message","topic"]);
        let encoded=serde_json::to_string_pretty(&graph).unwrap();
        let decoded:GraphDocument=serde_json::from_str(&encoded).unwrap();
        assert_eq!(graph,decoded);
        let names:Vec<_>=graph.nodes.iter().map(|n|(&n.id,&n.name,&n.class,&n.port_type,&n.parameters)).collect();
        assert!(names.iter().any(|(id,name,class,_,_)|id.as_str()=="K.validate_final_output" && !name.is_empty() && **class==NodeClass::Core));
        assert!(names.iter().any(|(id,_,class,_,_)|id.as_str()=="B.articulation" && **class==NodeClass::Behavior));
        assert!(validate_artifact("articulated_draft",&json!({"text":"hello"})));
        assert!(!validate_artifact("articulated_draft",&json!({"other":"hello"})));
    }
    #[test]
    fn bypassing_final_validator_is_rejected() {
        let bypass=edge("B.articulation","K.deliver",None,EdgeKind::Extension);
        assert_eq!(compile(profile(),&[],&[bypass],&[]).unwrap_err(),
            GraphError::SafetyBypass("K.validate_final_output".into()));
    }
    #[test]
    fn invalid_graphs_have_distinct_errors() {
        assert!(matches!(compile(profile(),&[],&[edge("K.deliver","B.articulation",None,EdgeKind::Extension)],&[]),Err(GraphError::Cycle)));
        assert!(matches!(compile(profile(),&[extra("B.orphan",&["ack"],&[])],&[],&[]),Err(GraphError::MissingProducer{..})));
        let mut dependent=extra("B.needs_model",&[],&[]);
        dependent.dependencies.push("ModelPort:style".into());
        assert!(matches!(compile(profile(),&[dependent],&[],&[]),Err(GraphError::MissingDependency{..})));
        assert!(matches!(compile(profile(),&[extra("B.second_draft",&[],&["articulated_draft"])],&[],&[]),Err(GraphError::ExclusiveProducer{..})));
        assert!(matches!(compile(profile(),&[extra("K.fake",&[],&[])],&[],&[]),Err(GraphError::FixedNodeOverride(_))));
        let mut misplaced=extra("B.misplaced",&[],&[]);
        misplaced.slot=Some("post_articulation_text_revision".into());
        let edges=[edge("B.selection","B.misplaced",None,EdgeKind::Extension),edge("B.misplaced","B.forecast",None,EdgeKind::Extension)];
        assert!(matches!(compile(profile(),&[misplaced],&edges,&[]),Err(GraphError::IllegalInsertion(_))));
    }
}
