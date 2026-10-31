import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";

function CredentialCardSkeleton() {
  return (
    <div className="flex min-w-0 flex-col gap-4 rounded-lg border p-3">
      <div className="min-w-0">
        <Skeleton className="h-6 w-48 max-w-full" />
        <Skeleton className="mt-1 h-5 w-64 max-w-full" />
      </div>
      <div className="flex flex-wrap gap-2">
        <Skeleton className="size-9" />
        <Skeleton className="size-9" />
      </div>
    </div>
  );
}

export function CredentialsSkeleton() {
  return (
    <div
      role="status"
      aria-label="Loading credentials"
      className="mt-2 grid grid-cols-[repeat(auto-fit,minmax(min(100%,24rem),1fr))] gap-2"
    >
      {Array.from({ length: 2 }, (_, index) => (
        <CredentialCardSkeleton key={index} />
      ))}
    </div>
  );
}

export function ProvidersSkeleton() {
  return (
    <div role="status" aria-label="Loading providers">
      {Array.from({ length: 2 }, (_, index) => (
        <Card key={index} className="mt-6 p-5">
          <div className="flex items-start justify-between gap-4">
            <div className="min-w-0 flex-1">
              <Skeleton className="h-7 w-40 max-w-full" />
              <Skeleton className="mt-1 h-5 w-56 max-w-full" />
            </div>
            <div className="flex shrink-0 gap-2">
              <Skeleton className="size-9" />
              <Skeleton className="size-9" />
            </div>
          </div>
          <section className="mt-4">
            <div className="flex flex-wrap items-center justify-between gap-2">
              <Skeleton className="h-6 w-28" />
              <Skeleton className="h-9 w-32" />
            </div>
            <div className="mt-2 grid grid-cols-[repeat(auto-fit,minmax(min(100%,24rem),1fr))] gap-2">
              <CredentialCardSkeleton />
            </div>
          </section>
        </Card>
      ))}
    </div>
  );
}
export function CredentialUsageSkeleton({
  balance,
  grouped,
}: {
  balance: boolean;
  grouped: boolean;
}) {
  if (balance) {
    return (
      <div role="status" aria-label="Loading usage">
        <Skeleton className="h-5 w-32" />
      </div>
    );
  }
  return (
    <div
      role="status"
      aria-label="Loading usage"
      className="space-y-3 @md:grid @md:grid-cols-[4.5rem_minmax(0,1fr)_8rem_minmax(11rem,max-content)] @md:gap-x-3"
    >
      {grouped && <Skeleton className="h-5 w-36 @md:col-span-4" />}
      <div className="grid min-w-0 grid-cols-[minmax(0,1fr)_auto] items-center gap-x-3 gap-y-1.5 text-sm @md:col-span-4 @md:grid-cols-subgrid">
        <Skeleton className="h-5 w-16 max-w-full" />
        <Skeleton className="col-span-2 row-start-2 h-2 w-full @md:col-span-1 @md:col-start-2 @md:row-start-1" />
        <Skeleton className="col-start-2 row-start-1 h-5 w-28 @md:col-start-3" />
        <Skeleton className="col-span-2 row-start-3 h-5 w-36 @md:col-span-1 @md:col-start-4 @md:row-start-1" />
      </div>
    </div>
  );
}
