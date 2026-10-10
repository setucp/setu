import type { components } from "./api-types";

export type Operator = components["schemas"]["OperatorOut"];
export type Site = components["schemas"]["SiteOut"];
export type SiteDetail = components["schemas"]["SiteDetailOut"];
export type SiteListItem = components["schemas"]["SiteListItemOut"];
export type SiteMetric = components["schemas"]["SiteMetricOut"];
export type Prompt = components["schemas"]["PromptOut"];
export type RevealOut = components["schemas"]["RevealOut"];
export type Server = components["schemas"]["ServerOut"];
export type RotateTokenResponse = components["schemas"]["RotateTokenResponse"];
export type AppInfo = components["schemas"]["AppOut"];
export type AppVersion = components["schemas"]["AppVersionOut"];
export type Job = components["schemas"]["JobOut"];
export type AuditEntry = components["schemas"]["AuditEntryOut"];
export type EnvEntry = components["schemas"]["EnvEntryOut"];
export type Backup = components["schemas"]["BackupOut"];
export type ManifestError = components["schemas"]["ManifestErrorOut"];
export type ValidateResponse = components["schemas"]["ValidateResponse"];
export type CreateServerResponse = components["schemas"]["CreateServerResponse"];
export type SiteJobResponse = components["schemas"]["SiteJobResponse"];
export type SetupStatus = components["schemas"]["SetupStatusOut"];
export type LocalServerResponse = components["schemas"]["LocalServerResponse"];

export interface PlanLimits {
  memory_mb?: number;
  cpu_percent?: number;
  disk_mb?: number;
}

/** `PlanOut` with its opaque `limits`/`allowed_app_ids` JSON narrowed. */
export type Plan = Omit<
  components["schemas"]["PlanOut"],
  "limits" | "allowed_app_ids"
> & {
  limits: PlanLimits;
  allowed_app_ids: string[];
};

export type CreateSiteResponse = components["schemas"]["CreateSiteResponse"];

export interface CreateSitePayload {
  name: string;
  app_id: string;
  version: string;
  plan_id: string;
  domain: string;
  owner_email: string;
  prompts: Record<string, string>;
  server_id?: string;
}

export interface PlanInput {
  name: string;
  limits: { memory_mb: number; cpu_percent: number; disk_mb: number };
  allowed_app_ids: string[];
}

interface ErrorEnvelope {
  error?: { code?: string; message?: string };
}

export class ApiError extends Error {
  readonly status: number;
  readonly code: string;

  constructor(status: number, code: string, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
  }
}

function toApiError(status: number, text: string): ApiError {
  try {
    const parsed = JSON.parse(text) as ErrorEnvelope | null;
    const err = parsed?.error;
    if (err !== null && typeof err === "object" && typeof err?.message === "string") {
      return new ApiError(
        status,
        typeof err.code === "string" ? err.code : "unknown",
        err.message,
      );
    }
  } catch {
    // Not JSON: fall through to the status-based message.
  }
  return new ApiError(status, "unknown", `request failed with ${status}`);
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(path, {
    credentials: "same-origin",
    headers: init?.body === undefined ? undefined : { "content-type": "application/json" },
    ...init,
  });
  if (!response.ok) {
    throw toApiError(response.status, await response.text());
  }
  if (response.status === 204) {
    return undefined as T;
  }
  return (await response.json()) as T;
}

function unwrap<T>(value: Record<string, T>, key: string): T {
  const out = value[key];
  if (out === undefined) throw new Error(`missing ${key} in response`);
  return out;
}

/** Typed surface over the generated schema. Every body is plain JSON. */
export const api = {
  login(email: string, password: string): Promise<Operator> {
    return request("/api/v1/auth/login", {
      method: "POST",
      body: JSON.stringify({ email, password }),
    });
  },

  logout(): Promise<void> {
    return request("/api/v1/auth/logout", { method: "POST" });
  },

  me(): Promise<Operator> {
    return request("/api/v1/auth/me");
  },

  async listSites(): Promise<SiteListItem[]> {
    const body = await request<{ sites: SiteListItem[] }>("/api/v1/sites");
    return body.sites;
  },

  getSite(id: string): Promise<SiteDetail> {
    return request(`/api/v1/sites/${encodeURIComponent(id)}`);
  },

  createSite(payload: CreateSitePayload): Promise<CreateSiteResponse> {
    const { server_id, ...rest } = payload;
    return request("/api/v1/sites", {
      method: "POST",
      body: JSON.stringify(
        server_id === undefined || server_id === "" ? rest : { ...rest, server_id },
      ),
    });
  },

  async listServers(): Promise<Server[]> {
    const body = await request<{ servers: Server[] }>("/api/v1/servers");
    return body.servers;
  },

  async listApps(): Promise<AppInfo[]> {
    const body = await request<{ apps: AppInfo[] }>("/api/v1/apps");
    return body.apps;
  },

  /** The `prompt` rules of one version: name, label, secret. Never a value. */
  async versionPrompts(appId: string, version: string): Promise<Prompt[]> {
    const body = await request<{ prompts: Prompt[] }>(
      `/api/v1/apps/${encodeURIComponent(appId)}/versions/${encodeURIComponent(version)}/prompts`,
    );
    return body.prompts;
  },

  async appVersions(appId: string): Promise<AppVersion[]> {
    const body = await request<{ versions: AppVersion[] }>(
      `/api/v1/apps/${encodeURIComponent(appId)}/versions`,
    );
    return body.versions;
  },

  async listPlans(): Promise<Plan[]> {
    const body = await request<{ plans: Plan[] }>("/api/v1/plans");
    return unwrap(body, "plans").map((p) => ({
      ...p,
      limits: (p.limits ?? {}) as PlanLimits,
      allowed_app_ids: (p.allowed_app_ids ?? []) as string[],
    }));
  },

  getJob(id: string): Promise<Job> {
    return request(`/api/v1/jobs/${encodeURIComponent(id)}`);
  },

  async listJobs(): Promise<Job[]> {
    const body = await request<{ jobs: Job[] }>("/api/v1/jobs?limit=200");
    return body.jobs;
  },

  getServer(id: string): Promise<Server> {
    return request(`/api/v1/servers/${encodeURIComponent(id)}`);
  },

  createServer(name: string): Promise<CreateServerResponse> {
    return request("/api/v1/servers", {
      method: "POST",
      body: JSON.stringify({ name }),
    });
  },

  /** Owner only: a new single-use join token (1 h) for a joined server. */
  rotateServerToken(id: string): Promise<RotateTokenResponse> {
    return request(`/api/v1/servers/${encodeURIComponent(id)}/rotate-token`, { method: "POST" });
  },

  cordonServer(id: string, cordoned: boolean): Promise<Server> {
    return request(`/api/v1/servers/${encodeURIComponent(id)}/cordon`, {
      method: "POST",
      body: JSON.stringify({ cordoned }),
    });
  },

  deleteServer(id: string): Promise<void> {
    return request(`/api/v1/servers/${encodeURIComponent(id)}`, {
      method: "DELETE",
    });
  },

  validateManifest(toml: string): Promise<ValidateResponse> {
    return request("/api/v1/apps/validate", {
      method: "POST",
      body: JSON.stringify({ toml }),
    });
  },

  publishApp(payload: { toml?: string; url?: string }): Promise<unknown> {
    return request("/api/v1/apps", {
      method: "POST",
      body: JSON.stringify(payload),
    });
  },

  createPlan(input: PlanInput): Promise<Plan> {
    return request("/api/v1/plans", {
      method: "POST",
      body: JSON.stringify(input),
    });
  },

  updatePlan(id: string, input: PlanInput): Promise<Plan> {
    return request(`/api/v1/plans/${encodeURIComponent(id)}`, {
      method: "PUT",
      body: JSON.stringify(input),
    });
  },

  async getSiteEnv(id: string): Promise<EnvEntry[]> {
    const body = await request<{ env: EnvEntry[] }>(
      `/api/v1/sites/${encodeURIComponent(id)}/env`,
    );
    return body.env;
  },

  putSiteEnv(id: string, env: Record<string, string>): Promise<SiteJobResponse> {
    return request(`/api/v1/sites/${encodeURIComponent(id)}/env`, {
      method: "PUT",
      body: JSON.stringify({ env }),
    });
  },

  /** Plaintext of one env value; the caller's password re-authenticates. */
  revealEnv(siteId: string, name: string, password: string): Promise<RevealOut> {
    return request(
      `/api/v1/sites/${encodeURIComponent(siteId)}/env/${encodeURIComponent(name)}/reveal`,
      { method: "POST", body: JSON.stringify({ password }) },
    );
  },

  async listBackups(siteId: string): Promise<Backup[]> {
    const body = await request<{ backups: Backup[] }>(
      `/api/v1/sites/${encodeURIComponent(siteId)}/backups`,
    );
    return body.backups;
  },

  takeBackup(siteId: string): Promise<{ job: Job }> {
    return request(`/api/v1/sites/${encodeURIComponent(siteId)}/backups`, {
      method: "POST",
    });
  },

  restoreBackup(siteId: string, backupId: string): Promise<{ job: Job }> {
    return request(
      `/api/v1/sites/${encodeURIComponent(siteId)}/backups/${encodeURIComponent(backupId)}/restore`,
      { method: "POST" },
    );
  },

  deleteBackup(siteId: string, backupId: string): Promise<{ job: Job }> {
    return request(
      `/api/v1/sites/${encodeURIComponent(siteId)}/backups/${encodeURIComponent(backupId)}`,
      { method: "DELETE" },
    );
  },

  siteAction(
    id: string,
    action: "start" | "stop" | "restart" | "retry",
  ): Promise<SiteJobResponse> {
    return request(`/api/v1/sites/${encodeURIComponent(id)}/${action}`, {
      method: "POST",
    });
  },

  updateSite(
    id: string,
    version: string,
    prompts: Record<string, string>,
  ): Promise<SiteJobResponse> {
    return request(`/api/v1/sites/${encodeURIComponent(id)}/update`, {
      method: "POST",
      body: JSON.stringify({ version, prompts }),
    });
  },

  deleteSite(id: string): Promise<{ job: Job }> {
    return request(`/api/v1/sites/${encodeURIComponent(id)}`, {
      method: "DELETE",
    });
  },

  async listOperators(): Promise<Operator[]> {
    const body = await request<{ operators: Operator[] }>("/api/v1/operators");
    return body.operators;
  },

  createOperator(email: string, password: string, role: string): Promise<Operator> {
    return request("/api/v1/operators", {
      method: "POST",
      body: JSON.stringify({ email, password, role }),
    });
  },

  /** Owner only. Refused for yourself and the last owner (409). */
  deleteOperator(id: string): Promise<void> {
    return request(`/api/v1/operators/${encodeURIComponent(id)}`, { method: "DELETE" });
  },

  /** Own password needs `current_password`; the owner sets anyone else's. */
  setOperatorPassword(
    id: string,
    password: string,
    currentPassword?: string,
  ): Promise<void> {
    return request(`/api/v1/operators/${encodeURIComponent(id)}/password`, {
      method: "PUT",
      body: JSON.stringify(
        currentPassword === undefined
          ? { password }
          : { current_password: currentPassword, password },
      ),
    });
  },

  async listAudit(): Promise<AuditEntry[]> {
    const body = await request<{ audit: AuditEntry[] }>("/api/v1/audit?limit=100");
    return body.audit;
  },

  setupStatus(): Promise<SetupStatus> {
    return request("/api/v1/setup");
  },

  setupOwner(token: string, email: string, password: string): Promise<Operator> {
    return request("/api/v1/setup", {
      method: "POST",
      body: JSON.stringify({ token, email, password }),
    });
  },

  setupLocalServer(name?: string): Promise<LocalServerResponse> {
    return request("/api/v1/setup/local-server", {
      method: "POST",
      body: JSON.stringify(name === undefined ? {} : { name }),
    });
  },
};
