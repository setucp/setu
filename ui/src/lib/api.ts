import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  api,
  type CreateSitePayload,
  type Operator,
  type PlanInput,
} from "./client";

export function useMe() {
  return useQuery({
    queryKey: ["me"],
    queryFn: () => api.me(),
    retry: false,
    staleTime: 60_000,
  });
}

export function useLogin() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ email, password }: { email: string; password: string }) =>
      api.login(email, password),
    onSuccess: (operator: Operator) => {
      queryClient.setQueryData(["me"], operator);
    },
  });
}

export function useLogout() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => api.logout(),
    onSuccess: () => {
      queryClient.setQueryData(["me"], null);
      void queryClient.invalidateQueries();
    },
  });
}

export function useSites() {
  return useQuery({ queryKey: ["sites"], queryFn: () => api.listSites() });
}

export function useSite(id: string) {
  return useQuery({ queryKey: ["sites", id], queryFn: () => api.getSite(id) });
}

export function useServers() {
  return useQuery({ queryKey: ["servers"], queryFn: () => api.listServers() });
}

export function useApps() {
  return useQuery({ queryKey: ["apps"], queryFn: () => api.listApps() });
}

export function useVersionPrompts(appId: string, version: string | null) {
  return useQuery({
    queryKey: ["apps", appId, "versions", version, "prompts"],
    queryFn: () => api.versionPrompts(appId, version ?? ""),
    enabled: version !== null && version !== "",
  });
}

export function useAppVersions(appId: string | null) {
  return useQuery({
    queryKey: ["apps", appId, "versions"],
    queryFn: () => api.appVersions(appId ?? ""),
    enabled: appId !== null && appId !== "",
  });
}

export function usePlans() {
  return useQuery({ queryKey: ["plans"], queryFn: () => api.listPlans() });
}

function jobIsOpen(status: string): boolean {
  return status === "queued" || status === "running";
}

export function useJob(id: string) {
  return useQuery({
    queryKey: ["jobs", id],
    queryFn: () => api.getJob(id),
    // Keep polling while the job is live; stop once it finishes.
    refetchInterval: (query) => {
      const data = query.state.data;
      return data !== undefined && jobIsOpen(data.status) ? 1000 : false;
    },
  });
}

export function useCreateSite() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (payload: CreateSitePayload) => api.createSite(payload),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["sites"] });
    },
  });
}

export function useServer(id: string) {
  return useQuery({ queryKey: ["servers", id], queryFn: () => api.getServer(id) });
}

export function useCreateServer() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (name: string) => api.createServer(name),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["servers"] });
    },
  });
}

export function useRotateServerToken(id: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => api.rotateServerToken(id),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["servers"] });
    },
  });
}

export function useCordonServer(id: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (cordoned: boolean) => api.cordonServer(id, cordoned),
    onSuccess: (server) => {
      queryClient.setQueryData(["servers", id], server);
      void queryClient.invalidateQueries({ queryKey: ["servers"] });
    },
  });
}

export function useDeleteServer() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.deleteServer(id),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["servers"] });
    },
  });
}

export function useValidateManifest() {
  return useMutation({
    mutationFn: (toml: string) => api.validateManifest(toml),
  });
}

export function usePublishApp() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (payload: { toml?: string; url?: string }) => api.publishApp(payload),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["apps"] });
    },
  });
}

export function useCreatePlan() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (input: PlanInput) => api.createPlan(input),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["plans"] });
    },
  });
}

export function useUpdatePlan() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ id, input }: { id: string; input: PlanInput }) =>
      api.updatePlan(id, input),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["plans"] });
    },
  });
}

export function useJobs() {
  return useQuery({
    queryKey: ["jobs"],
    queryFn: () => api.listJobs(),
    refetchInterval: 5000,
  });
}

export function useSiteEnv(siteId: string) {
  return useQuery({
    queryKey: ["sites", siteId, "env"],
    queryFn: () => api.getSiteEnv(siteId),
  });
}

export function usePutSiteEnv(siteId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (env: Record<string, string>) => api.putSiteEnv(siteId, env),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["sites", siteId, "env"] });
      void queryClient.invalidateQueries({ queryKey: ["jobs"] });
    },
  });
}

export function useBackups(siteId: string) {
  return useQuery({
    queryKey: ["sites", siteId, "backups"],
    queryFn: () => api.listBackups(siteId),
  });
}

export function useTakeBackup(siteId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => api.takeBackup(siteId),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["sites", siteId, "backups"] });
      void queryClient.invalidateQueries({ queryKey: ["jobs"] });
    },
  });
}

export function useRestoreBackup(siteId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (backupId: string) => api.restoreBackup(siteId, backupId),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["jobs"] });
    },
  });
}

export function useDeleteBackup(siteId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (backupId: string) => api.deleteBackup(siteId, backupId),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["sites", siteId, "backups"] });
      void queryClient.invalidateQueries({ queryKey: ["jobs"] });
    },
  });
}

/** Not cached: the plaintext lives only in the dialog that asked for it. */
export function useRevealEnv(siteId: string) {
  return useMutation({
    mutationFn: ({ name, password }: { name: string; password: string }) =>
      api.revealEnv(siteId, name, password),
  });
}

export function useSiteAction(siteId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (action: "start" | "stop" | "restart" | "retry") =>
      api.siteAction(siteId, action),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["sites", siteId] });
      void queryClient.invalidateQueries({ queryKey: ["sites"] });
      void queryClient.invalidateQueries({ queryKey: ["jobs"] });
    },
  });
}

export function useUpdateSite(siteId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ version, prompts }: { version: string; prompts: Record<string, string> }) =>
      api.updateSite(siteId, version, prompts),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["sites", siteId] });
      void queryClient.invalidateQueries({ queryKey: ["jobs"] });
    },
  });
}

export function useDeleteSite(siteId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => api.deleteSite(siteId),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["sites"] });
      void queryClient.invalidateQueries({ queryKey: ["jobs"] });
    },
  });
}

export function useOperators() {
  return useQuery({ queryKey: ["operators"], queryFn: () => api.listOperators() });
}

export function useCreateOperator() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({
      email,
      password,
      role,
    }: {
      email: string;
      password: string;
      role: string;
    }) => api.createOperator(email, password, role),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["operators"] });
    },
  });
}

export function useDeleteOperator() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (id: string) => api.deleteOperator(id),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["operators"] });
    },
  });
}

export function useSetOperatorPassword() {
  return useMutation({
    mutationFn: ({
      id,
      password,
      currentPassword,
    }: {
      id: string;
      password: string;
      currentPassword?: string;
    }) => api.setOperatorPassword(id, password, currentPassword),
  });
}

export function useSetupStatus() {
  return useQuery({ queryKey: ["setup"], queryFn: () => api.setupStatus() });
}

export function useAudit() {
  return useQuery({ queryKey: ["audit"], queryFn: () => api.listAudit() });
}

/** Control-plane liveness (`GET /healthz`), polled for the top-bar pill. */
export function useHealth() {
  return useQuery({
    queryKey: ["healthz"],
    queryFn: async (): Promise<{ status: string }> => {
      const response = await fetch("/healthz", { credentials: "same-origin" });
      if (!response.ok) throw new Error(`healthz ${response.status}`);
      return (await response.json()) as { status: string };
    },
    refetchInterval: 30_000,
    retry: false,
  });
}
