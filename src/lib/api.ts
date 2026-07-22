import { invoke } from "@tauri-apps/api/core";
import type {
  Memory,
  MemoryWithScore,
  Project,
  AgentEntry,
  ActivityEntry,
  Stats,
  IngestJob,
  ConsolidateReport,
  ConsolidateStatus,
  GraphData,
  BootstrapPayload,
  Operation,
  RecallResponse,
  MemoryCandidate,
  ObservationSource,
  CapturePolicy,
  HealthReport,
  IntegrityReport,
  MaintenancePolicy,
  AcceleratorStatus,
  RerankerStatus,
} from "./types";

export interface RememberInput {
  content: string;
  mem_type?: string | null;
  project_id?: string | null;
  tags?: string[] | null;
  importance?: number | null;
  source_agent?: string | null;
  supersedes?: string | null;
  file_path?: string | null;
  start_line?: number | null;
  end_line?: number | null;
  language?: string | null;
}

export interface UpdateInput {
  content?: string | null;
  mem_type?: string | null;
  tags?: string[] | null;
  importance?: number | null;
}

export const api = {
  ping: () => invoke<string>("ping"),

  listMemories: (params: {
    project_id?: string | null;
    mem_type?: string | null;
    limit?: number;
    offset?: number;
  }) =>
    invoke<Memory[]>("list_memories", {
      projectId: params.project_id ?? null,
      memType: params.mem_type ?? null,
      limit: params.limit ?? 50,
      offset: params.offset ?? 0,
    }),

  getMemory: (uid: string) =>
    invoke<Memory | null>("get_memory", { uid }),

  remember: (input: RememberInput) =>
    invoke<Memory>("remember", { input }),

  forget: (uid: string) =>
    invoke<boolean>("forget_memory", { uid }),

  update: (uid: string, input: UpdateInput) =>
    invoke<Memory>("update_memory", { uid, input }),

  search: (params: {
    project_id?: string | null;
    query: string;
    k?: number;
    mem_type?: string | null;
  }) =>
    invoke<MemoryWithScore[]>("search_memories", {
      args: {
        project_id: params.project_id ?? null,
        query: params.query,
        k: params.k ?? 10,
        mem_type: params.mem_type ?? null,
      },
    }),

  recallExplain: (params: {
    project_id?: string | null;
    query: string;
    k?: number;
    mem_type?: string | null;
  }) =>
    invoke<RecallResponse>("recall_explain", {
      args: {
        project_id: params.project_id ?? null,
        query: params.query,
        k: params.k ?? 10,
        mem_type: params.mem_type ?? null,
      },
    }),

  submitRecallFeedback: (
    recall_id: string,
    memory_uid: string,
    value: -1 | 1,
    source: "explicit" | "implicit" = "explicit",
  ) =>
    invoke<void>("submit_recall_feedback", {
      args: { recall_id, memory_uid, value, source },
    }),

  submitObservation: (input: {
    project_id: string;
    source_kind: "generic" | "codex" | "claude_code";
    external_id: string;
    session_id?: string | null;
    occurred_at: number;
    role: string;
    content: string;
    metadata?: Record<string, unknown> | null;
  }) => invoke<{ accepted: boolean; duplicate: boolean; observation_id: string; candidate_ids: string[]; warnings: string[] }>("submit_observation", { input }),

  listMemoryCandidates: (project_id?: string | null, status = "pending") =>
    invoke<MemoryCandidate[]>("list_memory_candidates", {
      args: { project_id: project_id ?? null, status, limit: 100, offset: 0 },
    }),
  getMemoryCandidate: (candidate_id: string) =>
    invoke<MemoryCandidate>("get_memory_candidate", { candidateId: candidate_id }),
  reviewMemoryCandidate: (input: {
    candidate_id: string;
    action: "approve" | "edit_and_approve" | "reject" | "merge" | "defer";
    edited_content?: string | null;
    target_memory_uid?: string | null;
    expected_version: number;
    decided_by?: string | null;
  }) => invoke<{ candidate: MemoryCandidate; decision_id: string; memory_uid: string | null; operation_id: string | null }>("review_memory_candidate", { input }),
  getCapturePolicy: (project_id: string) =>
    invoke<CapturePolicy>("get_capture_policy", { projectId: project_id }),
  updateCapturePolicy: (policy: CapturePolicy) =>
    invoke<CapturePolicy>("update_capture_policy", { policy }),
  listObservationSources: (project_id?: string | null) =>
    invoke<ObservationSource[]>("list_observation_sources", { projectId: project_id ?? null }),
  configureObservationSource: (source: ObservationSource) =>
    invoke<ObservationSource>("configure_observation_source", { source }),
  sourceStatus: (source_id: string) =>
    invoke<{ source: ObservationSource; checkpoint: Record<string, unknown> | null }>("source_status", { args: { source_id } }),
  startSourceSync: (source_id: string) =>
    invoke<Operation>("start_source_sync", { args: { source_id } }),

  healthReport: () => invoke<HealthReport>("health_report"),
  startIntegrityCheck: (project_id?: string | null) =>
    invoke<Operation>("start_integrity_check", { args: { project_id: project_id ?? null } }),
  integrityReport: (id?: string | null, project_id?: string | null) =>
    invoke<IntegrityReport | null>("integrity_report", { id: id ?? null, projectId: project_id ?? null }),
  repairIntegrity: (request: { project_id?: string | null; integrity_run_id: string; issue_ids: string[]; dry_run: boolean }) =>
    invoke<Operation | Record<string, unknown>>("repair_integrity", { request }),
  getMaintenancePolicy: (project_id: string) =>
    invoke<MaintenancePolicy>("get_maintenance_policy", { projectId: project_id }),
  updateMaintenancePolicy: (policy: MaintenancePolicy) =>
    invoke<MaintenancePolicy>("update_maintenance_policy", { policy }),

  acceleratorStatus: () => invoke<AcceleratorStatus>("accelerator_status"),
  getAcceleratorPreference: () => invoke<{ provider: string; environment_override?: string }>("get_accelerator_preference"),
  setAcceleratorPreference: (provider: "auto" | "cpu" | "cuda") =>
    invoke<{ provider: string; environment_override?: string }>("set_accelerator_preference", { provider }),
  rerankerStatus: () => invoke<RerankerStatus>("reranker_status"),
  startRerankerDownload: () => invoke<Operation>("start_reranker_download"),
  setRerankerEnabled: (enabled: boolean) =>
    invoke<RerankerStatus>("set_reranker_enabled", { enabled }),
  importRerankerArtifact: (path: string) =>
    invoke<RerankerStatus>("import_reranker_artifact", { path }),

  listProjects: () => invoke<Project[]>("list_projects"),
  getProject: (id: string) => invoke<Project>("get_project", { id }),
  listTags: (project_id?: string | null) =>
    invoke<[string, number][]>("list_tags", { projectId: project_id ?? null }),

  createProject: (input: {
    name: string;
    id?: string | null;
    description?: string | null;
    root_path?: string | null;
    bit_width?: number | null;
  }) =>
    invoke<Project>("create_project", {
      input: {
        name: input.name,
        id: input.id ?? null,
        description: input.description ?? null,
        root_path: input.root_path ?? null,
        bit_width: input.bit_width ?? null,
      },
    }),

  deleteProject: (id: string) => invoke<void>("delete_project", { id }),

  ensureProjectMarkerFiles: (project_id: string) =>
    invoke<{ project_id: string; created: string[] }>("ensure_project_marker_files", {
      projectId: project_id,
    }),

  ingestProject: (project_id: string, root_path: string) =>
    invoke<IngestJob>("ingest_project", {
      args: { project_id, root_path },
    }),

  startIngest: (project_id: string, root_path: string) =>
    invoke<Operation>("start_ingest", {
      args: { project_id, root_path },
    }),

  operationStatus: (id: string) => invoke<Operation>("operation_status", { id }),
  listOperations: (limit = 100) => invoke<Operation[]>("list_operations", { limit }),
  cancelOperation: (id: string) => invoke<Operation>("cancel_operation", { id }),
  retryOperation: (id: string) => invoke<Operation>("retry_operation", { id }),

  getProjectGraph: (project_id: string) =>
    invoke<GraphData>("get_project_graph", {
      args: { project_id },
    }),

  consolidate: (project_id?: string | null) =>
    invoke<ConsolidateReport>("consolidate_now", {
      projectId: project_id ?? null,
    }),

  consolidateStatus: () =>
    invoke<ConsolidateStatus>("consolidate_status"),

  importFolder: (project_id: string, root_path: string) =>
    invoke<{
      files_imported: number;
      memories_created: number;
      errors: string[];
    }>("import_folder", {
      args: { project_id, root_path },
    }),

  exportMemories: (project_id: string | null, output_path: string) =>
    invoke<{ memories_written: number; output_path: string }>("export_memories", {
      args: { project_id, output_path },
    }),

  setWatch: (project_id: string, root_path: string | null, enabled: boolean) =>
    invoke<{ enabled_projects: number; watching: string[] }>("set_watch", {
      args: { project_id, root_path, enabled },
    }),

  watchStatus: () =>
    invoke<{ enabled_projects: number; watching: string[] }>("watch_status"),

  setProjectEmbedModel: (project_id: string, model: string | null) =>
    invoke<void>("set_project_embed_model", {
      args: { project_id, model },
    }),

  stats: () => invoke<Stats>("stats"),
  listAgents: () => invoke<AgentEntry[]>("list_agents"),
  registerAgent: (name: string, kind: string, meta?: object) =>
    invoke<AgentEntry>("register_agent", {
      args: { name, kind, meta: meta ?? null },
    }),
  recentActivity: (limit = 100) =>
    invoke<ActivityEntry[]>("recent_activity", { limit }),

  bootstrap: () => invoke<BootstrapPayload>("bootstrap"),

  resolveMcpBinaryPath: () =>
    invoke<{ path: string; is_absolute: boolean }>("resolve_mcp_binary_path"),

  installMcpConfig: (target: string) =>
    invoke<{ target: string; path: string; created: boolean; merged: boolean }>(
      "install_mcp_config",
      { args: { target } },
    ),
};
