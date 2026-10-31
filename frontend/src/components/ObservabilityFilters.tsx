import { useState } from "react";
import {
  ChevronDown,
  PanelLeftClose,
  PanelLeftOpen,
  RotateCcw,
} from "lucide-react";
import { ScrollingText } from "@/components/ScrollingText";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import {
  Sheet,
  SheetContent,
  SheetTitle,
  SheetTrigger,
} from "@/components/ui/sheet";
import type { ObservabilityFilterValues } from "@/server/api/observability";

type FilterOptions = {
  providers?: string[];
  models?: string[];
  paths?: string[];
  virtual_keys?: { id: string; name: string }[];
  credentials?: { id: string; name: string }[];
};
type Props = {
  filters: ObservabilityFilterValues;
  options?: FilterOptions;
  onChange: (filters: ObservabilityFilterValues) => void;
};

function stringArray(value: unknown): string[] | undefined {
  return Array.isArray(value) &&
    value.every((item) => typeof item === "string") &&
    value.length
    ? value
    : undefined;
}

export function validateObservabilityFilters(
  search: Record<string, unknown>,
): ObservabilityFilterValues {
  const statuses = stringArray(search.statuses);
  return {
    providers: stringArray(search.providers),
    models: stringArray(search.models),
    paths: stringArray(search.paths),
    virtual_key_ids: stringArray(search.virtual_key_ids),
    credential_ids: stringArray(search.credential_ids),
    statuses: statuses?.every(
      (status) => status === "success" || status === "error",
    )
      ? (statuses as ObservabilityFilterValues["statuses"])
      : undefined,
  };
}

export function updateObservabilityFilters(
  current: Record<string, unknown>,
  next: ObservabilityFilterValues,
) {
  return {
    ...current,
    providers: next.providers?.length ? next.providers : undefined,
    models: next.models?.length ? next.models : undefined,
    paths: next.paths?.length ? next.paths : undefined,
    statuses: next.statuses?.length ? next.statuses : undefined,
    virtual_key_ids: next.virtual_key_ids?.length
      ? next.virtual_key_ids
      : undefined,
    credential_ids: next.credential_ids?.length
      ? next.credential_ids
      : undefined,
  };
}

function FilterGroup({
  title,
  values,
  selected,
  onChange,
  identities,
  defaultOpen = false,
}: {
  title: string;
  values: string[];
  selected: string[];
  onChange: (values: string[]) => void;
  identities?: { id: string; name: string }[];
  defaultOpen?: boolean;
}) {
  const [search, setSearch] = useState("");
  const [open, setOpen] = useState(defaultOpen || selected.length > 0);
  const names = new Map(identities?.map(({ id, name }) => [id, name]));
  const items = [...new Set([...selected, ...values])].filter((value) => {
    const query = search.toLocaleLowerCase();
    return (
      value.toLocaleLowerCase().includes(query) ||
      (names.get(value)?.toLocaleLowerCase().includes(query) ?? false)
    );
  });
  return (
    <details
      className="group"
      open={open}
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary className="flex h-8 cursor-pointer list-none items-center gap-1.5 px-2 text-sm font-medium hover:opacity-80 [&::-webkit-details-marker]:hidden">
        <ChevronDown className="size-3.5 -rotate-90 transition-transform group-open:rotate-0" />
        {title}{" "}
        {selected.length > 0 && (
          <span className="text-muted-foreground">({selected.length})</span>
        )}
      </summary>
      <div className="overflow-hidden rounded-sm border">
        {title !== "Status" && (
          <div className="border-b p-2">
            <Input
              aria-label={`Search ${title}`}
              placeholder={`Search ${title.toLowerCase()}...`}
              value={search}
              onChange={(event) => setSearch(event.target.value)}
              className="h-8"
            />
          </div>
        )}
        <div className="max-h-48 overflow-y-auto divide-y">
          {items.map((value) => (
            <label
              key={value}
              className="flex cursor-pointer items-center gap-2.5 px-3 py-2 text-sm hover:bg-muted/50"
            >
              <Checkbox
                checked={selected.includes(value)}
                onCheckedChange={(checked) =>
                  onChange(
                    checked
                      ? [...selected, value]
                      : selected.filter((item) => item !== value),
                  )
                }
              />
              {title === "Request paths" ? (
                <span className="min-w-0 flex-1">
                  <ScrollingText text={value} />
                </span>
              ) : (
                <span className="min-w-0 break-all">
                  {title === "Status"
                    ? value === "success"
                      ? "Successful"
                      : "Error"
                    : identities && names.has(value)
                      ? `${names.get(value)} (${value})`
                      : value}
                </span>
              )}
            </label>
          ))}
          {items.length === 0 && (
            <p className="px-3 py-2 text-sm text-muted-foreground">
              No options found
            </p>
          )}
        </div>
      </div>
    </details>
  );
}

function FilterContents({
  filters,
  options,
  onChange,
  onHide,
  mobile = false,
}: Props & { onHide: () => void; mobile?: boolean }) {
  const count = Object.values(filters).reduce(
    (total, values) => total + (values?.length ?? 0),
    0,
  );
  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex h-11 shrink-0 items-center justify-between border-b pl-5 pr-2">
        {mobile ? (
          <SheetTitle className="text-sm font-semibold">
            Filters{count > 0 && ` (${count})`}
          </SheetTitle>
        ) : (
          <span className="text-sm font-semibold">
            Filters{count > 0 && ` (${count})`}
          </span>
        )}
        <div className="flex items-center gap-1">
          {count > 0 && (
            <Button
              variant="outline"
              size="sm"
              className="h-7 px-2 text-xs"
              onClick={() => onChange({})}
            >
              <RotateCcw className="size-3" /> Reset
            </Button>
          )}
          <Button
            variant="ghost"
            size="icon"
            className="size-7"
            onClick={onHide}
            aria-label="Hide filters"
          >
            <PanelLeftClose className="size-4" />
          </Button>
        </div>
      </div>
      <div className="flex min-h-0 flex-col gap-1 overflow-y-auto p-2">
        <FilterGroup
          title="Status"
          values={["success", "error"]}
          selected={filters.statuses ?? []}
          onChange={(statuses) =>
            onChange({
              ...filters,
              statuses: statuses as ("success" | "error")[],
            })
          }
          defaultOpen
        />
        <FilterGroup
          title="Models"
          values={options?.models ?? []}
          selected={filters.models ?? []}
          onChange={(models) => onChange({ ...filters, models })}
        />
        <FilterGroup
          title="Providers"
          values={options?.providers ?? []}
          selected={filters.providers ?? []}
          onChange={(providers) => onChange({ ...filters, providers })}
        />
        <FilterGroup
          title="Virtual keys"
          values={(options?.virtual_keys ?? []).map(({ id }) => id)}
          identities={options?.virtual_keys ?? []}
          selected={filters.virtual_key_ids ?? []}
          onChange={(virtual_key_ids) =>
            onChange({ ...filters, virtual_key_ids })
          }
        />
        <FilterGroup
          title="Provider credentials"
          values={(options?.credentials ?? []).map(({ id }) => id)}
          identities={options?.credentials ?? []}
          selected={filters.credential_ids ?? []}
          onChange={(credential_ids) =>
            onChange({ ...filters, credential_ids })
          }
        />
        <FilterGroup
          title="Request paths"
          values={options?.paths ?? []}
          selected={filters.paths ?? []}
          onChange={(paths) => onChange({ ...filters, paths })}
        />
      </div>
    </div>
  );
}

export function ObservabilityFilters(props: Props) {
  const [visible, setVisible] = useState(true);
  const [mobileOpen, setMobileOpen] = useState(false);
  const count = Object.values(props.filters).reduce(
    (total, values) => total + (values?.length ?? 0),
    0,
  );
  return (
    <>
      <div className="sticky top-2 z-10 mb-3 self-start md:hidden">
        <Sheet open={mobileOpen} onOpenChange={setMobileOpen}>
          <SheetTrigger render={<Button variant="outline" size="sm" />}>
            <PanelLeftOpen className="size-4" /> Filters
            {count > 0 && ` (${count})`}
          </SheetTrigger>
          <SheetContent
            side="left"
            showCloseButton={false}
            className="w-72 max-w-[90vw] gap-0 p-0"
          >
            <FilterContents
              {...props}
              mobile
              onHide={() => setMobileOpen(false)}
            />
          </SheetContent>
        </Sheet>
      </div>
      <div className="hidden shrink-0 md:sticky md:top-4 md:block">
        {visible ? (
          <aside
            aria-label="Observability filters"
            className="flex max-h-[calc(100dvh-2rem)] w-64 flex-col overflow-hidden rounded-md border bg-card"
          >
            <FilterContents {...props} onHide={() => setVisible(false)} />
          </aside>
        ) : (
          <Button
            variant="outline"
            size="sm"
            onClick={() => setVisible(true)}
            aria-label={`Show filters${count ? ` (${count} active)` : ""}`}
          >
            <PanelLeftOpen className="size-4" /> Filters
            {count > 0 && ` (${count})`}
          </Button>
        )}
      </div>
    </>
  );
}
