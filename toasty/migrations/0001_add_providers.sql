CREATE TABLE "providers" (
    "id" TEXT NOT NULL,
    "name" TEXT NOT NULL,
    "kind" TEXT NOT NULL,
    "base_url" TEXT NOT NULL,
    "deleted" BOOLEAN NOT NULL,
    PRIMARY KEY ("id")
);
