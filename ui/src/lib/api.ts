import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { api, type CreateSitePayload, type Operator } from "./client";

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
