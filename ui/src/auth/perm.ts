// The permission catalog, mirroring `services/control/src/rbac.rs::CATALOG`,
// and the action → permission map, mirroring the guards on each handler in
// `services/control/src/api/`.
//
// The control plane enforces every one of these server-side; this exists so the
// console hides what a caller cannot do rather than offering them a button that
// returns 403. Keeping the mapping in one file is the point: a gate written
// inline at a call site is a gate that drifts from the handler it mirrors.

export const CATALOG = [
  "vm:create",
  "vm:read",
  "vm:update",
  "vm:delete",
  "vm:power",
  "vm:migrate",
  "vm:console",
  "host:read",
  "host:manage",
  "network:create",
  // Attaching a network to physical infrastructure (an uplink, a VLAN tag) is a
  // platform decision — admin only, deliberately not operator (ADR-016).
  "network:create:provider",
  "network:read",
  "network:update",
  "network:delete",
  // A storage pool is a platform resource like a host: read is broad,
  // management is admin-only (ADR-023).
  "storagepool:read",
  "storagepool:manage",
  "volume:create",
  "volume:read",
  "volume:update",
  "volume:delete",
  "image:create",
  "image:read",
  "image:update",
  "image:delete",
  "template:create",
  "template:read",
  "template:update",
  "template:delete",
  "iam:read",
  "iam:manage",
  // Tenancy boundaries are platform objects: creating or deleting one is not a
  // workload operation, and `operator` holds none of these beyond reading
  // (design §47, ADR-018).
  "project:read",
  "project:create",
  "project:update",
  "project:delete",
] as const;

export type Permission = (typeof CATALOG)[number];

/// Every mutating action the console offers, against the permission its
/// handler actually requires. Verified against the guards in
/// services/control/src/api/*.rs.
export const ACTION = {
  vmCreate: "vm:create",
  vmUpdate: "vm:update",
  vmDelete: "vm:delete",
  vmPower: "vm:power",
  vmMigrate: "vm:migrate",
  vmConsole: "vm:console",
  vmChangeNic: "vm:update",

  hostRegister: "host:manage",
  hostEnroll: "host:manage",
  hostCordon: "host:manage",
  hostDrain: "host:manage",

  networkCreate: "network:create",
  networkCreateProvider: "network:create:provider",
  networkUpdate: "network:update",
  networkDelete: "network:delete",

  // Security groups are guarded by the network permissions.
  sgCreate: "network:create",
  sgDelete: "network:delete",
  sgRuleAdd: "network:update",
  sgRuleDelete: "network:update",

  // Tenancy boundaries are platform objects, not workload ones: `operator`
  // reads projects but shapes none of them (design §47, ADR-018).
  projectCreate: "project:create",
  projectUpdate: "project:update",
  projectDelete: "project:delete",
  // Setting a quota is shaping the boundary, so it shares project:update.
  quotaSet: "project:update",

  poolCreate: "storagepool:manage",
  poolUpdate: "storagepool:manage",
  poolDelete: "storagepool:manage",

  volumeCreate: "volume:create",
  volumeUpdate: "volume:update",
  volumeDelete: "volume:delete",
  volumeAttach: "volume:update",
  volumeDetach: "volume:update",
  snapshotCreate: "volume:update",
  snapshotDelete: "volume:update",
  snapshotRevert: "volume:update",

  imageCreate: "image:create",
  imageImport: "image:create",
  imageUpload: "image:create",
  imageUpdate: "image:update",
  imageDelete: "image:delete",

  templateCreate: "template:create",
  templateUpdate: "template:update",
  templateDelete: "template:delete",

  iamRead: "iam:read",
  iamManage: "iam:manage",
} as const satisfies Record<string, Permission>;

/// The read permission each list query needs. A caller without it gets no
/// query at all rather than a 403 every poll interval.
export const READ = {
  vms: "vm:read",
  hosts: "host:read",
  networks: "network:read",
  securityGroups: "network:read",
  volumes: "volume:read",
  storagePools: "storagepool:read",
  images: "image:read",
  templates: "template:read",
  iam: "iam:read",
  // Tasks and events are guarded by vm:read server-side.
  tasks: "vm:read",
  events: "vm:read",
  projects: "project:read",
} as const satisfies Record<string, Permission>;
