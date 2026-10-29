import { useForm } from "@tanstack/react-form";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Fragment, useMemo, useState } from "react";
import { ChevronLeft, ChevronRight, Info } from "lucide-react";
import { antigravityReferencePrice } from "./modelPricing";
import { message } from "./server/api/client";
import {
  modelCatalogQueryOptions,
  modelDatasheetsQueryOptions,
  modelSettingsQueryOptions,
  refreshModelCatalog,
  saveModelSettings,
  syncModelSettings,
  type Datasheets,
  type Model,
  type Provider,
  type Settings,
} from "./server/api/models";
import { CardsSkeleton, FormSkeleton } from "./components/Loading";
import { RequestBody, requestBodyTheme } from "./RequestBody";
import { RefreshButton } from "./components/RefreshButton";
import { Alert, AlertDescription } from "./components/ui/alert";
import { Button } from "./components/ui/button";
import { Card } from "./components/ui/card";
import { Input } from "./components/ui/input";
import { Label } from "./components/ui/label";
import { Separator } from "./components/ui/separator";
import {
  Pagination,
  PaginationContent,
  PaginationItem,
} from "./components/ui/pagination";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "./components/ui/popover";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "./components/ui/select";

const pageClass =
  "mx-auto w-full min-w-0 max-w-7xl px-4 py-8 sm:px-6 lg:px-8 lg:py-12";
const inputClass = "mt-1 w-full";
const modelsPerPage = 12;

function lookup(
  sheet: Record<string, unknown> | undefined,
  provider: Provider,
  id: string,
): unknown {
  if (!sheet) return undefined;
  for (const key of [`${provider.id}/${id}`, `${provider.kind}/${id}`, id]) {
    const entry = sheet[key];
    if (!entry || typeof entry !== "object" || Array.isArray(entry)) continue;
    const owner = (entry as Record<string, unknown>).provider;
    if (
      typeof owner !== "string" ||
      (owner !== provider.id && owner !== provider.kind)
    )
      continue;
    if (key === id && id.includes("/") && !id.startsWith(`${owner}/`)) continue;
    return entry;
  }
  return undefined;
}

function JsonDetails({ title, value }: { title: string; value: unknown }) {
  const [open, setOpen] = useState(false);
  if (value === undefined || value === null) return null;
  return (
    <details
      className="mt-2 text-sm"
      onToggle={(event) => setOpen(event.currentTarget.open)}
    >
      <summary className="cursor-pointer font-medium">{title}</summary>
      {open && (
        <div className="mt-2">
          <RequestBody body={JSON.stringify(value)} />
        </div>
      )}
    </details>
  );
}

function modelName(model: Model): string {
  return typeof model.name === "string" && model.name.trim()
    ? model.name
    : typeof model.display_name === "string" && model.display_name.trim()
      ? model.display_name
      : model.id;
}

function ModelCard({
  model,
  provider,
  data,
}: {
  model: Model;
  provider: Provider;
  data?: Datasheets;
}) {
  const price =
    lookup(data?.pricing, provider, model.id) ??
    (provider.kind === "agy"
      ? antigravityReferencePrice(data?.pricing, model.id)
      : undefined);
  const parameters = lookup(data?.parameters, provider, model.id);
  const pricing =
    price && typeof price === "object" && !Array.isArray(price)
      ? (price as Record<string, unknown>)
      : {};
  const number = (value: unknown) =>
    typeof value === "number" && Number.isFinite(value)
      ? value.toLocaleString()
      : "—";
  const cost = (value: unknown) =>
    typeof value === "number" && Number.isFinite(value)
      ? `$${(value * 1_000_000).toLocaleString(undefined, { maximumFractionDigits: 6 })}`
      : "—";
  const title = modelName(model);
  const efforts = Array.isArray(model.supported_reasoning_efforts)
    ? model.supported_reasoning_efforts.filter(
        (value): value is string => typeof value === "string",
      )
    : [];
  const description =
    typeof model.description === "string" ? model.description : null;
  const rows = [
    ["Model ID", `${provider.id}/${model.id}`],
    ["Context window", number(model.context_window)],
    ["Max output", number(model.max_output_tokens)],
    ["Input price / 1M tokens", cost(pricing.input_cost_per_token)],
    ["Output price / 1M tokens", cost(pricing.output_cost_per_token)],
    ["Cache write / 1M tokens", cost(pricing.cache_creation_input_token_cost)],
    ["Cache read / 1M tokens", cost(pricing.cache_read_input_token_cost)],
  ];
  return (
    <Card className="min-w-0 gap-0 p-5">
      <h3 className="break-words text-lg font-semibold">{title}</h3>
      {description && (
        <p className="mt-1 break-words text-sm text-muted-foreground">
          {description}
        </p>
      )}
      <dl className="mt-4 divide-y text-sm">
        {efforts.length > 0 && (
          <div className="flex items-start justify-between gap-4 py-2">
            <dt className="shrink-0 text-muted-foreground">Reasoning</dt>
            <dd className="flex min-w-0 flex-wrap justify-end gap-1">
              {efforts.map((effort) => (
                <span
                  key={effort}
                  className="rounded-full border px-2 py-0.5 text-xs"
                >
                  {effort}
                </span>
              ))}
            </dd>
          </div>
        )}
        {rows.map(([label, value]) => (
          <div key={label} className="flex justify-between gap-4 py-2">
            <dt className="flex shrink-0 items-center gap-1 text-muted-foreground">
              {label}
              {provider.kind === "agy" &&
                label === "Input price / 1M tokens" && (
                  <Popover>
                    <PopoverTrigger
                      aria-label="About Antigravity reference pricing"
                      className="rounded-sm hover:text-foreground focus-visible:outline-2 focus-visible:outline-offset-2"
                    >
                      <Info className="size-3.5" />
                    </PopoverTrigger>
                    <PopoverContent>
                      These reference API prices come from the configured
                      pricing datasheet and are informational only. They are not
                      Antigravity charges; actual costs depend on your plan and
                      any credit overages.
                    </PopoverContent>
                  </Popover>
                )}
            </dt>
            <dd className="min-w-0 break-all text-right">{value}</dd>
          </div>
        ))}
      </dl>
      <div className="mt-2">
        <JsonDetails title="Model metadata" value={model} />
        <JsonDetails title="Pricing and limits" value={price} />
        <JsonDetails title="Supported parameters" value={parameters} />
      </div>
    </Card>
  );
}

function ProviderModels({
  provider,
  models,
  data,
}: {
  provider: Provider;
  models: Model[];
  data?: Datasheets;
}) {
  const [page, setPage] = useState(0);
  const total = models.length;
  const pageCount = Math.ceil(total / modelsPerPage);
  const currentPage = Math.min(page, Math.max(0, pageCount - 1));
  const first = currentPage * modelsPerPage;
  const pagination = (position: "top" | "bottom") =>
    pageCount > 1 && (
      <Pagination
        aria-label={`${provider.name} model pages (${position})`}
        className="m-0 w-auto flex-wrap items-center justify-between gap-3"
      >
        <p
          aria-live={position === "bottom" ? "polite" : "off"}
          className="text-sm text-muted-foreground"
        >
          {first + 1}–{Math.min(first + modelsPerPage, total)} of {total} models
        </p>
        <PaginationContent>
          <PaginationItem>
            <Button
              variant="ghost"
              size="sm"
              aria-label="Go to previous page"
              disabled={currentPage === 0}
              onClick={() => setPage(currentPage - 1)}
            >
              <ChevronLeft aria-hidden="true" /> Previous
            </Button>
          </PaginationItem>
          <PaginationItem>
            <span className="px-2 text-sm" aria-current="page">
              {currentPage + 1} / {pageCount}
            </span>
          </PaginationItem>
          <PaginationItem>
            <Button
              variant="ghost"
              size="sm"
              aria-label="Go to next page"
              disabled={currentPage + 1 >= pageCount}
              onClick={() => setPage(currentPage + 1)}
            >
              Next <ChevronRight aria-hidden="true" />
            </Button>
          </PaginationItem>
        </PaginationContent>
      </Pagination>
    );
  return (
    <section>
      <div className="flex flex-wrap items-center justify-between gap-3">
        <h2 className="text-lg font-semibold">{provider.name}</h2>
        {pagination("top")}
      </div>
      {provider.error && (
        <Alert variant="destructive" className="mt-3">
          <AlertDescription>{provider.error}</AlertDescription>
        </Alert>
      )}
      {models.length === 0 ? (
        <p className="mt-3 text-sm text-muted-foreground">No models found.</p>
      ) : (
        <div className="mt-3 grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
          {models.slice(first, first + modelsPerPage).map((model, index) => (
            <ModelCard
              key={`${model.id}-${index}`}
              model={model}
              provider={provider}
              data={data}
            />
          ))}
        </div>
      )}
      {pageCount > 1 && <div className="mt-5">{pagination("bottom")}</div>}
    </section>
  );
}

export function ModelCatalogPage() {
  const [search, setSearch] = useState("");
  const [providerFilter, setProviderFilter] = useState("");
  const queryClient = useQueryClient();
  const catalog = useQuery(modelCatalogQueryOptions());
  const data = useQuery(modelDatasheetsQueryOptions());
  const refresh = useMutation({
    mutationFn: refreshModelCatalog,
    onSuccess: async (result) => {
      await queryClient.cancelQueries({
        queryKey: modelCatalogQueryOptions().queryKey,
      });
      queryClient.setQueryData(modelCatalogQueryOptions().queryKey, result);
      await queryClient.invalidateQueries({
        queryKey: modelDatasheetsQueryOptions().queryKey,
      });
    },
  });
  const groups = useMemo(() => {
    const needle = search.trim().toLowerCase();
    return (catalog.data?.providers ?? [])
      .filter((provider) => !providerFilter || provider.id === providerFilter)
      .map((provider) => {
        const matchesProvider =
          `${provider.name} ${provider.id} ${provider.kind}`
            .toLowerCase()
            .includes(needle);
        const models = provider.models.filter(
          (model) =>
            matchesProvider ||
            [
              `${provider.id}/${model.id}`,
              model.id,
              model.name,
              model.display_name,
              model.default_reasoning_effort,
            ].some(
              (value) =>
                typeof value === "string" &&
                value.toLowerCase().includes(needle),
            ),
        );
        return { provider, models, matchesProvider };
      })
      .filter(
        ({ provider, models, matchesProvider }) =>
          !needle || matchesProvider || models.length > 0 || provider.error,
      );
  }, [catalog.data, providerFilter, search]);
  return (
    <main className={pageClass}>
      <style>{requestBodyTheme}</style>
      <div className="flex flex-wrap items-start justify-between gap-4">
        <div>
          <h1 className="text-3xl font-semibold tracking-tight">Models</h1>
          <p className="mt-2 text-sm text-muted-foreground">
            Discover models available from your configured providers.
          </p>
        </div>
        <RefreshButton
          loading={refresh.isPending}
          onClick={() => refresh.mutate()}
        />
      </div>
      {catalog.error && (
        <Alert variant="destructive" className="mt-5">
          <AlertDescription>{message(catalog.error)}</AlertDescription>
        </Alert>
      )}
      {refresh.error && (
        <Alert variant="destructive" className="mt-5">
          <AlertDescription>{message(refresh.error)}</AlertDescription>
        </Alert>
      )}
      {data.error && (
        <Alert variant="destructive" className="mt-5">
          <AlertDescription>
            Datasheets unavailable: {message(data.error)}
          </AlertDescription>
        </Alert>
      )}
      <div className="mt-6 grid gap-3 sm:grid-cols-2">
        <Label className="flex flex-col gap-2 items-start">
          Search models or providers
          <Input
            className={inputClass}
            value={search}
            onChange={(event) => setSearch(event.target.value)}
            placeholder="Search"
          />
        </Label>
        <div className="flex flex-col gap-2">
          <Label htmlFor="catalog-provider" className="block">
            Provider
          </Label>
          <Select
            items={[
              { value: "__all__", label: "All providers" },
              ...(catalog.data?.providers ?? []).map(({ id, name }) => ({
                value: id,
                label: name,
              })),
            ]}
            value={providerFilter || "__all__"}
            onValueChange={(value) =>
              setProviderFilter(value === "__all__" ? "" : (value ?? ""))
            }
          >
            <SelectTrigger id="catalog-provider" className="mt-1 w-full">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="__all__">All providers</SelectItem>
              {catalog.data?.providers.map((provider) => (
                <SelectItem key={provider.id} value={provider.id}>
                  {provider.name}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      </div>
      {catalog.isLoading && <CardsSkeleton label="Loading model catalog" />}
      {catalog.data && catalog.data.providers.length === 0 && (
        <p className="mt-6 text-sm text-muted-foreground">
          No providers in the catalog. Configure a provider and refresh to
          discover models.
        </p>
      )}
      {catalog.data &&
        catalog.data.providers.length > 0 &&
        groups.length === 0 && (
          <p className="mt-6 text-sm text-muted-foreground">
            No models match your search.
          </p>
        )}
      <div className="mt-6 grid gap-4">
        {groups.map(({ provider, models }, index) => (
          <Fragment key={`${provider.id}:${search}`}>
            {index > 0 && <Separator />}
            <ProviderModels
              provider={provider}
              models={models}
              data={data.data}
            />
          </Fragment>
        ))}
      </div>
    </main>
  );
}

function SettingsForm({ initial }: { initial: Settings }) {
  const queryClient = useQueryClient();
  const [status, setStatus] = useState("");
  const [error, setError] = useState("");
  const save = useMutation({
    mutationFn: saveModelSettings,
  });
  const sync = useMutation({
    mutationFn: syncModelSettings,
  });
  const form = useForm({
    defaultValues: initial,
    onSubmit: async ({ value }) => {
      setStatus("");
      setError("");
      if (
        ![value.pricing_interval_hours, value.discovery_interval_minutes].every(
          (n) => Number.isSafeInteger(n) && n >= 0,
        )
      ) {
        setError("Intervals must be non-negative whole numbers.");
        return;
      }
      try {
        const updated = await save.mutateAsync(value);
        await queryClient.cancelQueries({
          queryKey: modelSettingsQueryOptions().queryKey,
        });
        if (form.state.values.pricing_url === value.pricing_url) {
          form.setFieldValue("pricing_url", updated.pricing_url);
        }
        if (form.state.values.parameters_url === value.parameters_url) {
          form.setFieldValue("parameters_url", updated.parameters_url);
        }
        queryClient.setQueryData(modelSettingsQueryOptions().queryKey, updated);
        setStatus("Settings saved.");
      } catch (err) {
        setError(message(err));
      }
    },
  });
  async function forceSync() {
    setStatus("");
    setError("");
    try {
      await sync.mutateAsync();
      await queryClient.invalidateQueries({
        queryKey: modelDatasheetsQueryOptions().queryKey,
      });
      setStatus("Datasheets synced.");
    } catch (err) {
      setError(message(err));
    }
  }
  return (
    <>
      {error && (
        <Alert variant="destructive" className="mt-5">
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}
      {status && (
        <p role="status" className="mt-5 text-sm">
          {status}
        </p>
      )}
      <Card className="mt-6 p-5">
        <form
          className="grid gap-5"
          onSubmit={(event) => {
            event.preventDefault();
            void form.handleSubmit();
          }}
        >
          <form.Field name="pricing_url">
            {(field) => (
              <Label className="block">
                Pricing datasheet URL
                <Input
                  className={inputClass}
                  type="url"
                  value={field.state.value}
                  onBlur={field.handleBlur}
                  onChange={(event) => {
                    setStatus("");
                    field.handleChange(event.target.value);
                  }}
                />
              </Label>
            )}
          </form.Field>
          <form.Field name="parameters_url">
            {(field) => (
              <Label className="block">
                Parameters datasheet URL
                <Input
                  className={inputClass}
                  type="url"
                  value={field.state.value}
                  onBlur={field.handleBlur}
                  onChange={(event) => {
                    setStatus("");
                    field.handleChange(event.target.value);
                  }}
                />
              </Label>
            )}
          </form.Field>
          <p className="text-sm text-muted-foreground">
            Leave a datasheet URL blank to restore the default.
          </p>
          <div className="grid gap-5 sm:grid-cols-2">
            <form.Field name="pricing_interval_hours">
              {(field) => (
                <Label className="block">
                  Datasheet sync interval (hours)
                  <Input
                    className={inputClass}
                    type="number"
                    min="0"
                    step="1"
                    required
                    value={
                      Number.isNaN(field.state.value) ? "" : field.state.value
                    }
                    onBlur={field.handleBlur}
                    onChange={(event) => {
                      setStatus("");
                      field.handleChange(event.target.valueAsNumber);
                    }}
                  />
                </Label>
              )}
            </form.Field>
            <form.Field name="discovery_interval_minutes">
              {(field) => (
                <Label className="block">
                  Model discovery interval (minutes)
                  <Input
                    className={inputClass}
                    type="number"
                    min="0"
                    step="1"
                    required
                    value={
                      Number.isNaN(field.state.value) ? "" : field.state.value
                    }
                    onBlur={field.handleBlur}
                    onChange={(event) => {
                      setStatus("");
                      field.handleChange(event.target.valueAsNumber);
                    }}
                  />
                </Label>
              )}
            </form.Field>
          </div>
          <p className="text-sm text-muted-foreground">
            Set an interval to 0 to disable its automatic updates. Manual
            refresh and sync remain available.
          </p>
          <div className="flex flex-wrap gap-3">
            <Button type="submit" disabled={save.isPending || sync.isPending}>
              {save.isPending ? "Saving…" : "Save settings"}
            </Button>
            <Button
              type="button"
              variant="outline"
              disabled={save.isPending || sync.isPending}
              onClick={() => void forceSync()}
            >
              {sync.isPending ? "Syncing…" : "Sync datasheets now"}
            </Button>
          </div>
          <p className="text-sm text-muted-foreground">
            Sync uses saved settings. Save changes before syncing.
          </p>
        </form>
      </Card>
    </>
  );
}

export function ModelSettingsPage() {
  const settings = useQuery(modelSettingsQueryOptions());
  return (
    <main className={pageClass}>
      <h1 className="text-3xl font-semibold tracking-tight">Model settings</h1>
      <p className="mt-2 text-sm text-muted-foreground">
        Configure datasheets and automatic model discovery.
      </p>
      {settings.isLoading && <FormSkeleton label="Loading model settings" />}
      {settings.error && (
        <Alert variant="destructive" className="mt-5">
          <AlertDescription>{message(settings.error)}</AlertDescription>
        </Alert>
      )}
      {settings.data && <SettingsForm initial={settings.data} />}
    </main>
  );
}
