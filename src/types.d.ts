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
};
export type KvmMode = "server" | "client";
