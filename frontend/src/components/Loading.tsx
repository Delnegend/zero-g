import { Card } from "./ui/card";
import { Skeleton } from "./ui/skeleton";

function FieldSkeleton({ modelSettings }: { modelSettings: boolean }) {
  return (
    <div>
      <Skeleton className="h-5 w-40 max-w-full" />
      <Skeleton className={`mt-1 w-full ${modelSettings ? "h-8" : "h-9"}`} />
    </div>
  );
}

export function FormSkeleton({
  modelSettings = false,
}: {
  modelSettings?: boolean;
}) {
  return (
    <Card
      role="status"
      aria-label={modelSettings ? "Loading model settings" : "Loading settings"}
      className="mt-6 p-5"
    >
      <div className="grid gap-5">
        <FieldSkeleton modelSettings={modelSettings} />
        {modelSettings && (
          <>
            <FieldSkeleton modelSettings />
            <Skeleton className="h-5 w-80 max-w-full" />
            <div className="grid gap-5 sm:grid-cols-2">
              <FieldSkeleton modelSettings />
              <FieldSkeleton modelSettings />
            </div>
            <Skeleton className="h-5 w-full max-w-xl" />
          </>
        )}
        <div className="flex flex-wrap gap-3">
          <Skeleton className="h-8 w-28" />
          {modelSettings && <Skeleton className="h-8 w-40" />}
        </div>
        {modelSettings && <Skeleton className="h-5 w-80 max-w-full" />}
      </div>
    </Card>
  );
}

export function RowsSkeleton({ label }: { label: string }) {
  return (
    <div role="status" aria-label={label} className="divide-y">
      {Array.from({ length: 3 }, (_, index) => (
        <div
          key={index}
          className="flex flex-wrap items-center justify-between gap-4 py-4"
        >
          <div className="min-w-0 space-y-1">
            <Skeleton className="h-6 w-40 max-w-full" />
            <Skeleton className="h-5 w-64 max-w-full" />
          </div>
          <Skeleton className="h-8 w-32 shrink-0" />
        </div>
      ))}
    </div>
  );
}

export function CatalogSkeleton() {
  return (
    <section
      role="status"
      aria-label="Loading model catalog"
      className="@container mt-6 min-w-0"
    >
      <Skeleton className="h-7 w-44" />
      <div className="mt-3 grid grid-cols-1 gap-4 py-2 @min-[672px]:grid-cols-2 @min-[1008px]:grid-cols-3">
        {Array.from({ length: 3 }, (_, index) => (
          <Card
            key={index}
            className={`min-w-0 gap-0 p-5 ${index === 1 ? "hidden @min-[672px]:block" : index === 2 ? "hidden @min-[1008px]:block" : ""}`}
          >
            <Skeleton className="h-7 w-2/3" />
            <Skeleton className="mt-1 h-5 w-full" />
            <div className="mt-4 divide-y">
              {Array.from({ length: 8 }, (_, row) => (
                <div key={row} className="flex justify-between gap-4 py-2">
                  <Skeleton className="h-5 w-28 max-w-[50%]" />
                  <Skeleton className="h-5 w-20" />
                </div>
              ))}
            </div>
            <Skeleton className="mt-2 h-8 w-32" />
          </Card>
        ))}
      </div>
      <div className="flex justify-center gap-2">
        <Skeleton className="size-8 rounded-full" />
        <Skeleton className="size-8 rounded-full" />
      </div>
    </section>
  );
}
