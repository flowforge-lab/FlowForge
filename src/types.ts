// Add to existing types or create if not exists
export interface ToolContext {
  projectRoot: string;
  // ... other context properties ...
}

export interface Tool {
  name: string;
  description: string;
  parameters: Record<string, unknown>;
  execute(params: unknown, context: ToolContext): Promise<string>;
}