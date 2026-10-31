import { createFileRoute } from "@tanstack/react-router";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Fragment, useMemo, useRef, useState } from "react";
import {
  AudioLines,
  File,
  FileText,
  FileType,
  Image,
  Info,
  Video,
  type LucideIcon,
} from "lucide-react";
import { message } from "@/server/api/client";
import {
  modelCatalogQueryOptions,
  refreshModelCatalog,
  type CatalogModel,
  type Provider,
} from "@/server/api/models";
import { CatalogSkeleton } from "@/components/Loading";
import { RequestBody, requestBodyTheme } from "@/RequestBody";
import { RefreshButton } from "@/components/RefreshButton";
import { ScrollingText } from "@/components/ScrollingText";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Card } from "@/components/ui/card";
import {
  Carousel,
  CarouselContent,
  CarouselItem,
  CarouselNext,
  CarouselPrevious,
  useCarousel,
} from "@/components/ui/carousel";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Separator } from "@/components/ui/separator";
import {
  Tooltip,
  TooltipContent,
  TooltipTrigger,
} from "@/components/ui/tooltip";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "@/components/ui/popover";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

export const Route = createFileRoute("/models/catalog")({
  head: () => ({ meta: [{ title: "Catalog · Zero-G" }] }),
  component: ModelCatalogPage,
});

const pageClass =
  "mx-auto w-full min-w-0 max-w-7xl px-4 py-8 sm:px-6 lg:px-8 lg:py-12";
const inputClass = "mt-1 w-full";

const modalityIcons: Record<string, LucideIcon> = {
  text: FileText,
  file: File,
  image: Image,
  audio: AudioLines,
  video: Video,
  pdf: FileType,
};

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

function ModelCard({
  model,
  provider,
}: {
  model: CatalogModel;
  provider: Provider;
}) {
  const descriptionRef = useRef<HTMLButtonElement>(null);
  const rawModel = model.model;
  const architecture = rawModel.architecture;
  const outputModalities =
    architecture &&
    typeof architecture === "object" &&
    "output_modalities" in architecture
      ? architecture.output_modalities
      : undefined;
  const modalities =
    Array.isArray(outputModalities) &&
    outputModalities.every((value) => typeof value === "string")
      ? outputModalities
      : null;
  const endpoints =
    Array.isArray(rawModel.endpoints) &&
    rawModel.endpoints.every((value) => typeof value === "string")
      ? rawModel.endpoints
      : null;
  const hasDecisions =
    modalities?.includes("decisions") ||
    endpoints?.some((value) => value.endsWith("/decisions"));
  const hasOtherOutputs = modalities?.some((value) => value !== "decisions");
  const hasOtherEndpoints = endpoints?.some(
    (value) => !value.endsWith("/decisions"),
  );
  const decisionsOnly =
    modalities?.length &&
    modalities.every((value) => value === "decisions") &&
    !hasOtherEndpoints;
  const decisionsLabel = hasDecisions
    ? decisionsOnly
      ? "Decisions API only (not Chat Completions or Responses)"
      : hasOtherOutputs || hasOtherEndpoints
        ? "Decisions API and other capabilities"
        : "Decisions API supported"
    : null;
  const number = (value: unknown) =>
    typeof value === "number" && Number.isFinite(value)
      ? value.toLocaleString()
      : "—";
  const cost = (value: unknown) =>
    typeof value === "number" && Number.isFinite(value)
      ? `$${value.toLocaleString(undefined, { maximumFractionDigits: 6 })}`
      : "—";
  const title = model.display_name;
  const efforts = model.reasoning_efforts;
  const description = model.description;
  const pricingNote =
    model.pricing?.source === "antigravity-reference"
      ? "These reference API prices come from the configured pricing datasheet and are informational only. They are not Antigravity charges; actual costs depend on your plan and any credit overages."
      : model.pricing?.source === "claude-reference"
        ? "These are Anthropic API reference prices from the configured datasheet, not Claude subscription charges. Actual costs depend on your plan."
        : model.pricing?.source === "opencode-go"
          ? "These base rates count toward your OpenCode Go subscription allowance. Conditions such as time-of-day multipliers can change the effective rate; see pricing details."
          : model.pricing?.source === "opencode-zen"
            ? "These are OpenCode Zen base rates. Conditional pricing and other charges may apply; see pricing details."
            : model.pricing?.source === "openrouter"
              ? "These are OpenRouter base token prices. Provider routing, pricing overrides, and other charges can change the actual cost; see pricing details."
              : null;
  const capabilities = [
    ...(typeof model.reasoning === "boolean"
      ? [["Reasoning support", model.reasoning ? "Yes" : "No"]]
      : []),
    ...(typeof model.tool_call === "boolean"
      ? [["Tool calling", model.tool_call ? "Yes" : "No"]]
      : []),
  ];
  const rows = [
    ...(decisionsLabel ? [["API", decisionsLabel]] : []),
    ["Model ID", `${provider.id}/${rawModel.id}`],
    ["Context window", number(rawModel.context_window)],
    ["Max output", number(rawModel.max_output_tokens)],
    ["Input price / 1M tokens", cost(model.pricing?.input)],
    ["Output price / 1M tokens", cost(model.pricing?.output)],
    ["Cache write / 1M tokens", cost(model.pricing?.cache_write)],
    ["Cache read / 1M tokens", cost(model.pricing?.cache_read)],
    ...capabilities,
  ];
  return (
    <Card className="min-w-0 gap-0 p-5">
      <h3 className="min-w-0 text-lg font-semibold">
        <ScrollingText text={title} />
      </h3>
      {description && (
        <Tooltip
          onOpenChange={(open, details) => {
            const element = descriptionRef.current;
            if (
              open &&
              (!element || element.scrollHeight <= element.clientHeight)
            ) {
              details.cancel();
            }
          }}
        >
          <TooltipTrigger
            ref={descriptionRef}
            className="mt-1 line-clamp-3 break-words text-left text-sm text-muted-foreground focus-visible:outline-2 focus-visible:outline-offset-2"
          >
            {description}
          </TooltipTrigger>
          <TooltipContent>
            <span className="block max-h-[50vh] overflow-y-auto whitespace-pre-wrap break-words">
              {description}
            </span>
          </TooltipContent>
        </Tooltip>
      )}
      <dl className="mt-4 divide-y text-sm">
        {efforts.length > 0 && (
          <div className="flex flex-wrap items-start justify-between gap-x-4 gap-y-1 py-2">
            <dt className="min-w-0 text-muted-foreground">Reasoning</dt>
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
          <div
            key={label}
            className="flex flex-wrap justify-between gap-x-4 gap-y-1 py-2"
          >
            <dt className="flex min-w-0 items-center gap-1 text-muted-foreground">
              {label}
              {pricingNote && label === "Input price / 1M tokens" && (
                <Popover>
                  <PopoverTrigger
                    aria-label={
                      model.pricing?.source === "antigravity-reference"
                        ? "About Antigravity reference pricing"
                        : "About model pricing"
                    }
                    className="rounded-sm hover:text-foreground focus-visible:outline-2 focus-visible:outline-offset-2"
                  >
                    <Info className="size-3.5" />
                  </PopoverTrigger>
                  <PopoverContent>{pricingNote}</PopoverContent>
                </Popover>
              )}
            </dt>
            <dd className="min-w-16 flex-1 text-right">
              {label === "Model ID" || label === "API" ? (
                <ScrollingText text={value} align="right" />
              ) : (
                <span className="break-all">{value}</span>
              )}
            </dd>
          </div>
        ))}
        {model.input_modalities.length > 0 && (
          <div className="flex flex-wrap justify-between gap-x-4 gap-y-1 py-2">
            <dt className="flex min-w-0 items-center gap-1 text-muted-foreground">
              Input modalities
              {provider.kind === "agy" &&
                model.input_modalities.some((value) =>
                  ["audio", "video", "pdf"].includes(value),
                ) && (
                  <Popover>
                    <PopoverTrigger
                      aria-label="About Antigravity input support"
                      className="rounded-sm hover:text-foreground focus-visible:outline-2 focus-visible:outline-offset-2"
                    >
                      <Info className="size-3.5" />
                    </PopoverTrigger>
                    <PopoverContent>
                      These types are advertised by Antigravity. Zero-G forwards
                      inline audio, video, and PDF data to compatible models;
                      external URLs and uploaded file IDs are not supported.
                    </PopoverContent>
                  </Popover>
                )}
              {provider.kind === "claude" &&
                model.input_modalities.includes("pdf") && (
                  <Popover>
                    <PopoverTrigger
                      aria-label="About Claude input support"
                      className="rounded-sm hover:text-foreground focus-visible:outline-2 focus-visible:outline-offset-2"
                    >
                      <Info className="size-3.5" />
                    </PopoverTrigger>
                    <PopoverContent>
                      Anthropic advertises PDF input for this model, but Zero-G
                      currently forwards only text and images to Claude.
                    </PopoverContent>
                  </Popover>
                )}
            </dt>
            <dd className="flex min-w-0 flex-wrap justify-end gap-2">
              {model.input_modalities.map((modality, index) => {
                const key = modality.toLowerCase();
                const Icon = Object.hasOwn(modalityIcons, key)
                  ? modalityIcons[key]
                  : undefined;
                if (!Icon)
                  return <span key={`${modality}-${index}`}>{modality}</span>;
                return (
                  <Tooltip key={`${modality}-${index}`}>
                    <TooltipTrigger
                      aria-label={`${modality} input`}
                      className="rounded-sm text-muted-foreground hover:text-foreground focus-visible:outline-2 focus-visible:outline-offset-2"
                    >
                      <Icon aria-hidden="true" className="size-4" />
                    </TooltipTrigger>
                    <TooltipContent>{modality}</TooltipContent>
                  </Tooltip>
                );
              })}
            </dd>
          </div>
        )}
      </dl>
      <div className="mt-2">
        <JsonDetails title="Model metadata" value={rawModel} />
        <JsonDetails
          title="Models.dev metadata"
          value={model.provider_metadata}
        />
        <JsonDetails title="Pricing and limits" value={model.raw_pricing} />
        <JsonDetails title="Supported parameters" value={model.parameters} />
      </div>
    </Card>
  );
}
function CarouselEdgeFades() {
  const { canScrollPrev, canScrollNext } = useCarousel();
  return (
    <>
      <div
        aria-hidden="true"
        className={`pointer-events-none absolute inset-y-0 left-0 w-4 bg-gradient-to-r from-background to-transparent transition-opacity duration-200 motion-reduce:transition-none ${canScrollPrev ? "opacity-100" : "opacity-0"}`}
      />
      <div
        aria-hidden="true"
        className={`pointer-events-none absolute inset-y-0 right-0 w-4 bg-gradient-to-l from-background to-transparent transition-opacity duration-200 motion-reduce:transition-none ${canScrollNext ? "opacity-100" : "opacity-0"}`}
      />
    </>
  );
}

function ProviderModels({
  provider,
  models,
}: {
  provider: Provider;
  models: CatalogModel[];
}) {
  return (
    <section className="@container min-w-0">
      <h2 className="text-lg font-semibold">
        {provider.name}
        {models.length > 0 && (
          <>
            {" "}
            <span className="text-sm font-normal text-muted-foreground">
              {models.length} {models.length === 1 ? "model" : "models"}
            </span>
          </>
        )}
      </h2>
      {provider.error && (
        <Alert variant="destructive" className="mt-3">
          <AlertDescription>{provider.error}</AlertDescription>
        </Alert>
      )}
      {models.length === 0 ? (
        <p className="mt-3 text-sm text-muted-foreground">No models found.</p>
      ) : (
        <Carousel
          className="mt-3 min-w-0"
          aria-label={`${provider.name} model cards`}
          opts={{ align: "start" }}
        >
          <div className="relative py-2">
            <CarouselContent>
              {models.map((model, index) => (
                <CarouselItem
                  key={`${model.model.id}-${index}`}
                  className="basis-full @min-[672px]:basis-1/2 @min-[1008px]:basis-1/3 [&>div]:h-full"
                >
                  <ModelCard model={model} provider={provider} />
                </CarouselItem>
              ))}
            </CarouselContent>
            <CarouselEdgeFades />
          </div>
          <div className="flex justify-center gap-2">
            <CarouselPrevious className="static" />
            <CarouselNext className="static" />
          </div>
        </Carousel>
      )}
    </section>
  );
}

export function ModelCatalogPage() {
  const [search, setSearch] = useState("");
  const [providerFilter, setProviderFilter] = useState("");
  const queryClient = useQueryClient();
  const catalog = useQuery(modelCatalogQueryOptions());
  const refresh = useMutation({
    mutationFn: refreshModelCatalog,
    onSuccess: async (result) => {
      await queryClient.cancelQueries({
        queryKey: modelCatalogQueryOptions().queryKey,
      });
      queryClient.setQueryData(modelCatalogQueryOptions().queryKey, result);
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
              `${provider.id}/${model.model.id}`,
              model.model.id,
              model.model.name,
              model.model.display_name,
              model.display_name,
              model.model.default_reasoning_effort,
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
      {catalog.data?.datasheets_error && (
        <Alert variant="destructive" className="mt-5">
          <AlertDescription>{catalog.data.datasheets_error}</AlertDescription>
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
      {catalog.isLoading && <CatalogSkeleton />}
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
            <ProviderModels provider={provider} models={models} />
          </Fragment>
        ))}
      </div>
    </main>
  );
}
