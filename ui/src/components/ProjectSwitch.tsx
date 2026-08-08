// The project switcher (design §47, ADR-020).
//
// Its own file rather than a helper inside the shell: it is the one control
// that changes what every other screen is showing, and it is the piece worth
// testing on its own.

import { useState } from "react";
import Menu from "@mui/material/Menu";
import MenuItem from "@mui/material/MenuItem";
import { PLATFORM, useProject } from "../auth/ProjectProvider";
import { useProjects } from "../api/hooks";

/// The project the console is acting in (design §47).
///
/// Hidden entirely when tenancy is off: a selector with one immutable option is
/// a question the operator does not need to be asked. The platform view is
/// offered only to callers who hold a platform-wide binding, because for anyone
/// else it resolves to no permissions at all — an option that answers 403 is
/// worse than no option.
export function ProjectSwitch() {
  const { project, setProject, enabled, canPlatform } = useProject();
  const { data: projects } = useProjects(enabled);
  const [anchor, setAnchor] = useState<null | HTMLElement>(null);
  if (!enabled) return null;

  const current =
    project === PLATFORM
      ? "All projects"
      : (projects?.find((p) => p.id === project)?.name ??
        projects?.find((p) => p.is_default)?.name ??
        "default");

  const choose = (next: string | null) => {
    setAnchor(null);
    setProject(next);
  };

  return (
    <>
      <button className="vq-project" onClick={(e) => setAnchor(e.currentTarget)}>
        <span className="vq-project-label">Project</span>
        {current}
      </button>
      <Menu anchorEl={anchor} open={!!anchor} onClose={() => setAnchor(null)}>
        {canPlatform && (
          <MenuItem selected={project === PLATFORM} onClick={() => choose(PLATFORM)}>
            All projects
          </MenuItem>
        )}
        {(projects ?? []).map((p) => (
          <MenuItem
            key={p.id}
            selected={project === p.id || (project === null && p.is_default)}
            onClick={() => choose(p.id)}
          >
            {p.name}
          </MenuItem>
        ))}
        {!projects?.length && <MenuItem disabled>No projects visible</MenuItem>}
      </Menu>
    </>
  );
}
