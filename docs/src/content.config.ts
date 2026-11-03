import { defineCollection } from "astro:content";
import { z } from "astro/zod";
import { docsCollection } from "@cloudflare/nimbus-docs/content";

export const collections = {
  docs: defineCollection(
    docsCollection({
      schemaFields: {
        // Set `audience: human` to flag a page written primarily for human
        // readers rather than agents.
        audience: z.literal("human").optional(),
        // Decision records (ADRs and RFCs) carry their own metadata.
        id: z.string().optional(),
        status: z.string().optional(),
        decision_date: z.coerce.date().optional(),
        approvers: z.array(z.string()).optional(),
        related_features: z.array(z.string()).optional(),
        supersedes: z.array(z.string()).optional(),
        superseded_by: z.array(z.string()).optional(),
      },
    }),
  ),
};
