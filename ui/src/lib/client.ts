import type { components } from "./api-types";

export type Operator = components["schemas"]["OperatorOut"];
export type Site = components["schemas"]["SiteOut"];
export type Server = components["schemas"]["ServerOut"];
export type AppInfo = components["schemas"]["AppOut"];
export type AppVersion = components["schemas"]["AppVersionOut"];
export type Job = components["schemas"]["JobOut"];
export type AuditEntry = components["schemas"]["AuditEntryOut"];

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

  async listSites(): Promise<Site[]> {
    const body = await request<{ sites: Site[] }>("/api/v1/sites");
    return body.sites;
  },

  getSite(id: string): Promise<Site> {
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
};
