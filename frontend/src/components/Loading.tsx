import { Card } from "./ui/card";
import { Skeleton } from "./ui/skeleton";

export function FormSkeleton({ label }: { label: string }) {
  return (
    <Card role="status" aria-label={label} className="mt-6 grid gap-5 p-5">
      {Array.from({ length: 3 }, (_, index) => (
        <div key={index} className="space-y-2">
          <Skeleton className="h-4 w-40 max-w-full" />
          <Skeleton className="h-9 w-full" />
        </div>
      ))}
      <Skeleton className="h-8 w-32" />
    </Card>
  );
}

export function RowsSkeleton({ label }: { label: string }) {
  return (
    <div role="status" aria-label={label} className="grid gap-5">
      {Array.from({ length: 3 }, (_, index) => (
        <div key={index} className="flex items-center justify-between gap-4">
          <div className="min-w-0 flex-1 space-y-2">
            <Skeleton className="h-4 w-40 max-w-full" />
            <Skeleton className="h-3 w-64 max-w-full" />
          </div>
          <Skeleton className="h-8 w-20 shrink-0" />
        </div>
      ))}
    </div>
  );
}

export function CardsSkeleton({ label }: { label: string }) {
  return (
    <div role="status" aria-label={label} className="mt-6 grid gap-4">
      {Array.from({ length: 2 }, (_, index) => (
        <Card key={index} className="grid gap-4 p-5">
          <Skeleton className="h-5 w-48 max-w-full" />
          <Skeleton className="h-4 w-72 max-w-full" />
          <Skeleton className="h-9 w-28" />
        </Card>
      ))}
    </div>
  );
}

export function ObservabilitySkeleton({ logsOnly }: { logsOnly: boolean }) {
  return (
    <div
      role="status"
      aria-label={logsOnly ? "Loading request logs" : "Loading dashboard"}
    >
      {!logsOnly && (
        <>
          <div className="mb-8 grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
            {Array.from({ length: 4 }, (_, index) => (
              <Card key={index} className="grid gap-4 p-5">
                <Skeleton className="h-4 w-28" />
                <Skeleton className="h-7 w-20" />
                <Skeleton className="h-3 w-32" />
              </Card>
            ))}
          </div>
          <Card className="mb-8 grid gap-4 p-5">
            <Skeleton className="h-4 w-40" />
            <Skeleton className="h-3 w-72 max-w-full" />
            <Skeleton className="h-[280px] w-full" />
          </Card>
        </>
      )}
      <Card className="grid gap-5 p-5">
        <Skeleton className="h-5 w-36" />
        <div aria-hidden="true">
          <RowsSkeleton label="Loading request logs" />
        </div>
      </Card>
    </div>
  );
}
