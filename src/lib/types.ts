export type MemType =
  | "fact"
  | "decision"
  | "preference"
  | "pattern"
  | "episode"
  | "reflection"
  | "code";

export interface Memory {
  uid: string;
  project_id: string;
  mem_type: string;
  content: string;
  tags: string[];
  source_agent: string | null;
  importance: number;
  supersedes: number | null;
  superseded_by: number | null;
  created_at: number;
  updated_at: number;
  last_access: number;
  access_count: number;
  file_path: string | null;
  start_line: number | null;
  end_line: number | null;
  language: string | null;
}

export interface MemoryWithScore extends Memory {
  score: number;
}

export interface RecallExplanation {
  vector_rank: number | null;
  fts_rank: number | null;
  matched_terms: string[];
  feedback_boost: number;
  reciprocal_rank_contribution?: number;
  recency_contribution?: number;
  explicit_feedback_boost?: number;
  implicit_feedback_boost?: number;
  staleness_penalty?: number;
  contradiction_penalty?: number;
  pre_reranker_score?: number;
  reranker_applied?: boolean;
  active_filters?: string[];
}

export interface ExplainedMemory extends MemoryWithScore {
  explanation: RecallExplanation;
}

export interface RecallResponse {
  recall_id: string;
  results: ExplainedMemory[];
}

export interface Project {
  id: string;
  name: string;
  description: string | null;
  root_path: string | null;
  bit_width: number;
  dim: number;
  memory_count: number;
  indexed_count: number;
  embed_model: string | null;
  watch_enabled: boolean;
}

export interface AgentEntry {
  id: string;
  name: string;
  kind: string;
  last_seen: number;
  created_at: number;
  meta: Record<string, unknown> | null;
}

export interface ActivityEntry {
  id: number;
  project_id: string | null;
  agent_id: string | null;
  action: string;
  memory_uid: string | null;
  detail: Record<string, unknown> | null;
  created_at: number;
}

export interface Stats {
  total_memories: number;
  total_projects: number;
  total_agents: number;
  by_type: [string, number][];
  by_project: [string, number][];
  index_bytes: number;
  recent_writes_7d: number;
  recent_reads_7d: number;
}

export interface IngestResult {
  project_id: string;
  files_indexed: number;
  chunks_indexed: number;
  bytes_processed: number;
  languages: Record<string, number>;
  errors: string[];
  edges_created: number;
}

export interface IngestJob {
  job_id: string;
  project_id: string;
}

export interface IngestProgress {
  project_id: string;
  phase: string;
  current: number;
  total: number;
  file: string | null;
  chunks_so_far: number;
}

export interface IngestDone {
  job_id: string;
  project_id: string;
  files_indexed: number;
  chunks_indexed: number;
  edges_created: number;
  elapsed_ms: number;
}

export interface IngestError {
  job_id: string;
  project_id: string;
  error: string;
}

export interface Operation {
  id: string;
  kind: string;
  project_id: string | null;
  status: "queued" | "running" | "succeeded" | "failed" | "cancelled";
  phase: string | null;
  current: number;
  total: number;
  checkpoint: Record<string, unknown> | null;
  result: Record<string, unknown> | null;
  error: string | null;
  cancel_requested: boolean;
  created_at: number;
  updated_at: number;
  started_at: number | null;
  finished_at: number | null;
}

export interface ConsolidateReport {
  decayed: number;
  duplicates_found: number;
  merged: number;
  removed: number;
}

export interface ConsolidateStatus {
  last_run_at: number | null;
  next_run_in_secs: number;
  last_report: ConsolidateReport | null;
  running: boolean;
  interval_secs: number;
  queued?: boolean;
}

export interface BootstrapPayload {
  stats: Stats;
  projects: Project[];
  recent: ActivityEntry[];
  tags: [string, number][];
  agents: AgentEntry[];
  consolidate: ConsolidateStatus;
}

export interface GraphNode {
  uid: string;
  label: string;
  kind: string; // "file" | "function" | "class" | "struct" | "module"
  file_path: string | null;
  start_line: number | null;
  end_line: number | null;
  language: string | null;
  size: number;
}

export interface GraphEdge {
  from: string;
  to: string;
  edge_type: string; // "member_of" | "imports" | "calls" | "extends"
  weight: number;
}

export interface GraphData {
  project_id: string;
  nodes: GraphNode[];
  edges: GraphEdge[];
}

export interface CandidateEvidence {
  excerpt: string;
  source_pointer: string | null;
  source_timestamp: number;
  evidence_hash: string;
  extraction_method: string;
}

export interface MemoryCandidate {
  id: string;
  observation_id: string;
  project_id: string;
  content: string;
  mem_type: string;
  tags: string[];
  confidence: number;
  status: string;
  duplicate_memory_uid: string | null;
  contradiction_uid: string | null;
  resulting_memory_uid: string | null;
  version: number;
  evidence: CandidateEvidence[];
  created_at: number;
  updated_at: number;
}

export interface ObservationSource {
  id: string;
  project_id: string;
  kind: "generic" | "codex" | "claude_code";
  name: string;
  root_path: string | null;
  enabled: boolean;
  config: Record<string, unknown>;
  last_sync_at: number | null;
  last_error: string | null;
  processed_count: number;
  candidate_count: number;
  created_at: number;
  updated_at: number;
}

export interface CapturePolicy {
  project_id: string;
  enabled_sources: string[];
  allowed_categories: string[];
  extraction_mode: "deterministic" | "ollama";
  ollama_endpoint: string;
  ollama_model: string | null;
  approval_mode: "review_required" | "trusted_categories";
  auto_approve_categories: string[];
  evidence_max_chars: number;
  redaction_mode: string;
  notify_candidates: boolean;
  updated_at: number;
}

export interface IntegrityIssue {
  id: string;
  run_id: string;
  project_id: string | null;
  severity: string;
  subsystem: string;
  issue_kind: string;
  expected_state: string;
  actual_state: string;
  recommended_action: string;
  safe_automatic: boolean;
  repaired_at: number | null;
  created_at: number;
}

export interface IntegrityReport {
  id: string;
  project_id: string | null;
  trigger_kind: string;
  status: string;
  checked_projects: number;
  issue_count: number;
  repaired_count: number;
  deferred_count: number;
  before_summary: Record<string, unknown>;
  after_summary: Record<string, unknown> | null;
  started_at: number;
  finished_at: number | null;
  issues: IntegrityIssue[];
}

export interface HealthReport {
  status: "healthy" | "degraded" | "critical";
  checked_at: number;
  project_count: number;
  pending_mutations: number;
  pending_candidates: number;
  recoverable_operations: number;
  expired_leases: number;
  last_integrity_run: IntegrityReport | null;
}

export interface MaintenancePolicy {
  project_id: string;
  enabled: boolean;
  interval_hours: number;
  idle_delay_seconds: number;
  auto_safe_repairs: boolean;
  last_run_at: number | null;
  next_run_at: number | null;
  updated_at: number;
}

export interface AcceleratorStatus {
  compiled_providers: string[];
  requested_provider: string;
  effective_provider: string;
  cuda_available: boolean;
  initialization_error: string | null;
  onnx_runtime_version: string;
  model_name: string;
  model_dimension: number;
  model_cache_present: boolean;
  last_inference_provider: string;
  fallback_count: number;
  warmup_ms: number;
  average_batch_ms: number;
}

export interface RerankerStatus {
  model_name: string;
  version: string;
  license: string;
  size_bytes: number;
  sha256: string;
  installed: boolean;
  enabled: boolean;
  loaded: boolean;
  artifact_path: string;
  last_error: string | null;
  last_recall_applied: boolean;
}
