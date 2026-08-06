// Tasks (handoff §11). A failed task is followed by its own full-width error
// row — never a dismissible alert, never detached from the row it belongs to.
// The message is the whole reason an operator opened this screen.

import { useMemo, useState } from "react";
import { Link } from "react-router-dom";
import { useTasks, useVms } from "../api/hooks";
import {
  Dash,
  EmptyState,
  Pagination,
  PageHeader,
  ProgressCell,
  QueryError,
  SkeletonRows,
  StateChip,
  Table,
  THead,
  TRow,
} from "../ui/kit";
import { duration, formatTime } from "../format";

const COLS = "1fr 1.3fr 1.3fr 1.6fr 1fr 1fr";
const PAGE_SIZE = 25;

export function Tasks() {
  const tasks = useTasks();
  const vms = useVms();
  const [page, setPage] = useState(1);

  const vmById = useMemo(() => {
    const m = new Map<string, string>();
    (vms.data ?? []).forEach((v) => m.set(v.id, v.name));
    return m;
  }, [vms.data]);

  const list = tasks.data ?? [];
  // Task history grows without bound; render a page of it, not all of it.
  const pages = Math.max(1, Math.ceil(list.length / PAGE_SIZE));
  const shown = list.slice((page - 1) * PAGE_SIZE, page * PAGE_SIZE);

  return (
    <>
      <PageHeader
        title="Tasks"
        subtitle="Every mutating operation is a persisted task. Failed tasks retain their last state and error."
      />

      <QueryError error={tasks.error} what="tasks" />

      <Table>
        <THead cols={COLS}>
          <div>Task ID</div>
          <div>Type</div>
          <div>Resource</div>
          <div>State</div>
          <div>Started</div>
          <div>Duration</div>
        </THead>

        {tasks.isLoading && <SkeletonRows cols={COLS} />}

        {!tasks.isLoading && list.length === 0 && (
          <div style={{ padding: 18 }}>
            <EmptyState
              headline="No tasks yet"
              hint="Create or power a VM and the work shows up here."
            />
          </div>
        )}

        {shown.map((t) => {
          const vmName = t.vm_id ? vmById.get(t.vm_id) : undefined;
          const failed = t.state === "Failed";
          return (
            <div key={t.id}>
              <TRow cols={COLS} tint={failed ? "red" : undefined}>
                <div className="vq-cell vq-mono-sm t-blue">{t.id.slice(0, 12)}</div>
                <div className="vq-cell vq-mono">{t.task_type}</div>
                <div className="vq-cell">
                  {t.vm_id ? (
                    <Link className="vq-name" to={`/vms/${t.vm_id}`}>
                      {vmName ?? t.vm_id.slice(0, 8)}
                    </Link>
                  ) : (
                    <Dash />
                  )}
                </div>
                <div>
                  {t.state === "Running" ? (
                    <ProgressCell
                      pct={t.progress}
                      width={56}
                      label={t.message ?? `${t.progress}%`}
                    />
                  ) : (
                    <StateChip value={t.state} dense />
                  )}
                </div>
                <div className="vq-mono-sm">{formatTime(t.created_at)}</div>
                <div className="vq-mono-sm">{duration(t.created_at, t.updated_at)}</div>
              </TRow>
              {failed && t.message && (
                <div className="vq-errorrow">
                  {t.id.slice(0, 12)} · {t.message}
                </div>
              )}
            </div>
          );
        })}

        {list.length > 0 && (
          <Pagination
            page={page}
            pages={pages}
            shown={shown.length}
            total={list.length}
            onPage={setPage}
          />
        )}
      </Table>
    </>
  );
}
