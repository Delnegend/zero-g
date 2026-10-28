import { useMemo, useState } from "react";
import {
  flexRender,
  getCoreRowModel,
  getFilteredRowModel,
  getSortedRowModel,
  useReactTable,
  type ColumnDef,
  type SortingState,
} from "@tanstack/react-table";
import { ArrowUpDown, Search } from "lucide-react";
import type { Log } from "./App";
import { Button } from "./components/ui/button";
import { Card } from "./components/ui/card";
import { Input } from "./components/ui/input";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "./components/ui/table";

const number = new Intl.NumberFormat();

function formatDate(value: number): string {
  const date = new Date(value * 1000);
  return Number.isNaN(date.getTime()) ? String(value) : date.toLocaleString();
}

export function LogsTable({
  logs,
  selectedId,
  onSelect,
}: {
  logs: Log[];
  selectedId: Log["id"] | null;
  onSelect: (id: Log["id"]) => void;
}) {
  const [query, setQuery] = useState("");
  const [sorting, setSorting] = useState<SortingState>([]);
  const columns = useMemo<ColumnDef<Log>[]>(
    () => [
      {
        accessorKey: "created_at",
        header: "Time",
        cell: ({ row }) => formatDate(row.original.created_at),
      },
      {
        accessorKey: "model",
        header: "Provider / model",
        cell: ({ row }) => (
          <>
            <div className="font-medium">{row.original.model}</div>
            <div className="text-xs text-muted-foreground">
              {row.original.provider}
            </div>
          </>
        ),
      },
      {
        accessorKey: "path",
        header: "Path",
        cell: ({ row }) => (
          <span className="font-mono text-xs">{row.original.path}</span>
        ),
      },
      {
        accessorKey: "status",
        header: "Status",
        cell: ({ row }) => (
          <span
            className={`rounded-md px-2 py-1 text-xs font-medium ${row.original.status >= 400 ? "bg-destructive/10 text-destructive" : "bg-muted text-foreground"}`}
          >
            {row.original.status}
          </span>
        ),
      },
      {
        accessorKey: "latency_ms",
        header: "Latency",
        cell: ({ row }) => `${number.format(row.original.latency_ms)} ms`,
      },
      {
        id: "details",
        header: () => <span className="sr-only">Details</span>,
        enableSorting: false,
        cell: ({ row }) => (
          <Button
            variant="outline"
            size="sm"
            aria-label={`View request ${row.original.id}`}
            aria-pressed={selectedId === row.original.id}
            onClick={() => onSelect(row.original.id)}
          >
            View
          </Button>
        ),
      },
    ],
    [selectedId, onSelect],
  );
  const table = useReactTable({
    data: logs,
    columns,
    state: { sorting, globalFilter: query.trim() },
    onSortingChange: setSorting,
    globalFilterFn: (row, _columnId, filterValue: string) => {
      const term = filterValue.toLowerCase();
      const log = row.original;
      return [
        log.id,
        log.created_at,
        log.provider,
        log.model,
        log.path,
        log.status,
      ].some((value) =>
        String(value ?? "")
          .toLowerCase()
          .includes(term),
      );
    },
    getCoreRowModel: getCoreRowModel(),
    getFilteredRowModel: getFilteredRowModel(),
    getSortedRowModel: getSortedRowModel(),
  });
  const rows = table.getRowModel().rows;

  return (
    <Card aria-labelledby="logs-heading" className="min-w-0 py-0">
      <div className="flex flex-wrap items-end justify-between gap-4 border-b p-5">
        <div>
          <h2 id="logs-heading" className="text-lg font-semibold">
            Request logs
          </h2>
          <p className="mt-1 text-sm text-muted-foreground">
            Select a request to inspect its details.
          </p>
        </div>
        <div className="relative w-full sm:w-72">
          <Search
            aria-hidden="true"
            className="pointer-events-none absolute left-3 top-1/2 size-4 -translate-y-1/2 text-muted-foreground"
          />
          <Input
            type="search"
            aria-label="Search request logs"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
            placeholder="Search logs…"
            className="h-9 w-full rounded-lg border border-input bg-background pl-9 pr-3 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50"
          />
        </div>
      </div>
      <div className="px-5 pt-4 sm:hidden">
        <label className="flex items-center gap-3 text-sm">
          Sort by
          <select
            className="min-w-0 rounded-md border border-input bg-background px-2 py-1 text-foreground"
            value={
              sorting.length
                ? `${sorting[0].id}:${sorting[0].desc ? "desc" : "asc"}`
                : ""
            }
            onChange={(event) => {
              const [id, direction] = event.target.value.split(":");
              setSorting(id ? [{ id, desc: direction === "desc" }] : []);
            }}
          >
            <option value="">Recent first</option>
            <option value="created_at:desc">Time: newest</option>
            <option value="created_at:asc">Time: oldest</option>
            <option value="model:asc">Model: A–Z</option>
            <option value="model:desc">Model: Z–A</option>
            <option value="status:asc">Status: low to high</option>
            <option value="status:desc">Status: high to low</option>
            <option value="latency_ms:asc">Latency: low to high</option>
            <option value="latency_ms:desc">Latency: high to low</option>
            <option value="path:asc">Path: A–Z</option>
            <option value="path:desc">Path: Z–A</option>
          </select>
        </label>
      </div>
      <div className="sm:hidden">
        {rows.map((row) => {
          const log = row.original;
          return (
            <article
              key={row.id}
              className="min-w-0 border-b p-5 last:border-0"
            >
              <div className="flex min-w-0 items-start justify-between gap-3">
                <div className="min-w-0">
                  <p className="break-all font-medium">{log.model}</p>
                  <p className="break-all text-xs text-muted-foreground">
                    {log.provider}
                  </p>
                </div>
                <Button
                  variant="outline"
                  size="sm"
                  aria-label={`View request ${log.id}`}
                  aria-pressed={selectedId === log.id}
                  onClick={() => onSelect(log.id)}
                >
                  View
                </Button>
              </div>
              <p className="mt-3 break-all font-mono text-xs text-muted-foreground">
                {log.path}
              </p>
              <div className="mt-3 flex flex-wrap items-center gap-x-3 gap-y-1 text-xs">
                <span className="text-muted-foreground">
                  {formatDate(log.created_at)}
                </span>
                <span
                  className={`rounded-md px-2 py-1 font-medium ${log.status >= 400 ? "bg-destructive/10 text-destructive" : "bg-muted text-foreground"}`}
                >
                  {log.status}
                </span>
                <span>{number.format(log.latency_ms)} ms</span>
              </div>
            </article>
          );
        })}
      </div>
      <div className="hidden min-w-0 overflow-x-auto sm:block">
        <Table className="w-full min-w-[720px] text-left text-sm">
          <TableHeader className="border-b text-xs text-muted-foreground">
            {table.getHeaderGroups().map((group) => (
              <TableRow key={group.id}>
                {group.headers.map((header) => {
                  const sorted = header.column.getIsSorted();
                  return (
                    <TableHead
                      key={header.id}
                      className="px-5 py-3 font-medium"
                      scope="col"
                      aria-sort={
                        sorted === "asc"
                          ? "ascending"
                          : sorted === "desc"
                            ? "descending"
                            : undefined
                      }
                    >
                      {header.column.getCanSort() ? (
                        <Button
                          variant="ghost"
                          size="sm"
                          className="-ml-3"
                          onClick={header.column.getToggleSortingHandler()}
                        >
                          {flexRender(
                            header.column.columnDef.header,
                            header.getContext(),
                          )}
                          <ArrowUpDown aria-hidden="true" className="size-4" />
                        </Button>
                      ) : (
                        flexRender(
                          header.column.columnDef.header,
                          header.getContext(),
                        )
                      )}
                    </TableHead>
                  );
                })}
              </TableRow>
            ))}
          </TableHeader>
          <TableBody>
            {rows.map((row) => (
              <TableRow
                key={row.id}
                className="border-b last:border-0 hover:bg-muted/50"
              >
                {row.getVisibleCells().map((cell) => (
                  <TableCell
                    key={cell.id}
                    className={`px-5 py-4 ${cell.column.id === "created_at" ? "whitespace-nowrap text-muted-foreground" : cell.column.id === "latency_ms" ? "whitespace-nowrap" : ""}`}
                  >
                    {flexRender(cell.column.columnDef.cell, cell.getContext())}
                  </TableCell>
                ))}
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </div>
      <div>
        {rows.length === 0 && (
          <p className="px-5 py-10 text-center text-sm text-muted-foreground">
            {logs.length === 0
              ? "No requests recorded yet."
              : "No requests match your search."}
          </p>
        )}
      </div>
    </Card>
  );
}
