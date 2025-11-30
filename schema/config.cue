package schema

#Config: {
        // Task output format
        outputFormat?: "tui" | "spinner" | "simple" | "tree" | "json"

        // Task execution backend configuration
        backend?: {
                // Which backend to use by default for tasks
                type: *"host" | "dagger" | string

                // Backend-specific options (opaque to core)
                options?: {
                        // For Dagger backend
                        image?: string
                        platform?: string
                }
        }
}