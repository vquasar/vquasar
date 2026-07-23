import Chip from "@mui/material/Chip";
import type { ChipProps } from "@mui/material/Chip";

type Color = ChipProps["color"];

function colorFor(value: string): Color {
  switch (value) {
    case "Running":
    case "Ready":
    case "Succeeded":
      return "success";
    case "Failed":
    case "NotReady":
      return "error";
    case "Stopped":
    case "Disabled":
    case "Cancelled":
      return "default";
    case "Maintenance":
      return "warning";
    default:
      // Transitional states (Creating, Scheduling, Starting, Stopping, Pending, Running task…)
      return "info";
  }
}

export function StatusChip({ value }: { value: string }) {
  return <Chip label={value} color={colorFor(value)} size="small" variant="filled" />;
}
