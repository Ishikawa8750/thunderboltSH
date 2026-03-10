export type DeviceInfo = {
  hostname: string;
  os: string;
  ip: string;
  apiPort: number;
  kvmPort: number;
};

export type SystemOverview = {
  appVersion: string;
  platform: string;
  localIp?: string;
  discovered: DeviceInfo[];
  syncRunning: boolean;
  apiAuthEnabled: boolean;
};

export type KvmMode = "server" | "client";

export type SyncLogEntry = {
  ts: number;
  level: string;
  action: string;
  path: string;
  message: string;
};

export type SyncStats = {
  total: number;
  success: number;
  failed: number;
  conflicts: number;
  retries: number;
};

export type RetryQueueStatus = {
  pending: number;
  storePath: string;
};

export type RetryQueueItem = {
  id: number;
  attempts: number;
  kind: string;
  target: string;
};

export type SyncDirection = "outbound" | "bidirectional";
