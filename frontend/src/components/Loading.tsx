import { Card, CardContent, CardHeader } from "./ui/card";
import { Skeleton } from "./ui/skeleton";

/** Placeholder matching the settings page: two cards, then the save button. */
export function FormSkeleton() {
  return (
    <div role="status" aria-label="Loading settings" className="grid gap-6">
      {[1, 2].map((fields) => (
        <Card key={fields} className="gap-6 [--card-spacing:--spacing(6)]">
          <CardHeader className="gap-2">
            <Skeleton className="h-6 w-48 max-w-full" />
            <Skeleton className="h-4 w-full max-w-xl" />
          </CardHeader>
          <CardContent className="grid gap-4 sm:grid-cols-2">
            {Array.from({ length: fields }, (_, index) => (
              <div key={index} className="grid gap-2">
                <Skeleton className="h-4 w-40 max-w-full" />
                <Skeleton className="h-10 w-full" />
              </div>
            ))}
          </CardContent>
        </Card>
      ))}
      <Skeleton className="h-8 w-32" />
    </div>
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
