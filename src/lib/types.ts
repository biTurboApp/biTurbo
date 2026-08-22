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
