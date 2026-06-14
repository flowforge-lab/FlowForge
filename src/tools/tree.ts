import { readdirSync, statSync } from 'fs';
import { join, relative, resolve, sep } from 'path';
import { Tool, ToolContext } from '../types';

interface TreeOptions {
  path?: string;
  depth?: number;
  showHidden?: boolean;
  respectGitignore?: boolean;
}

const DEFAULT_DEPTH = 3;
const MAX_DEPTH = 10;

/**
 * Recursively builds a tree string for a given directory.
 * Jailed to project root — cannot traverse above it.
 */
function buildTree(
  dirPath: string,
  jailRoot: string,
  currentDepth: number,
  maxDepth: number,
  prefix: string = '',
  showHidden: boolean = false,
  gitignorePatterns: string[] = []
): string {
  if (currentDepth > maxDepth) {
    return `${prefix}└── ... (max depth reached)\n`;
  }

  let result = '';
  let entries: string[];

  try {
    entries = readdirSync(dirPath);
  } catch {
    return `${prefix}└── [error: cannot read directory]\n`;
  }

  // Filter hidden files unless explicitly requested
  if (!showHidden) {
    entries = entries.filter(e => !e.startsWith('.'));
  }

  // Filter gitignored files if patterns provided
  if (gitignorePatterns.length > 0) {
    entries = entries.filter(e => {
      return !gitignorePatterns.some(pattern => {
        if (pattern.endsWith('/')) {
          return e === pattern.slice(0, -1);
        }
        return e === pattern || e.endsWith('/' + pattern);
      });
    });
  }

  // Sort: directories first, then files, both alphabetically
  entries.sort((a, b) => {
    const aIsDir = statSync(join(dirPath, a)).isDirectory();
    const bIsDir = statSync(join(dirPath, b)).isDirectory();
    if (aIsDir && !bIsDir) return -1;
    if (!aIsDir && bIsDir) return 1;
    return a.localeCompare(b);
  });

  for (let i = 0; i < entries.length; i++) {
    const entry = entries[i];
    const fullPath = join(dirPath, entry);
    const isLast = i === entries.length - 1;
    const connector = isLast ? '└── ' : '├── ';
    const nextPrefix = isLast ? '    ' : '│   ';

    // Jail check: resolve to absolute and ensure it's within jailRoot
    const resolvedPath = resolve(fullPath);
    if (!resolvedPath.startsWith(jailRoot)) {
      continue; // skip entries outside jail
    }

    try {
      const stats = statSync(fullPath);
      if (stats.isDirectory()) {
        result += `${prefix}${connector}${entry}/\n`;
        result += buildTree(
          fullPath,
          jailRoot,
          currentDepth + 1,
          maxDepth,
          prefix + nextPrefix,
          showHidden,
          gitignorePatterns
        );
      } else {
        result += `${prefix}${connector}${entry}\n`;
      }
    } catch {
      result += `${prefix}${connector}${entry} [error: cannot stat]\n`;
    }
  }

  return result;
}

/**
 * Loads .gitignore patterns from a directory (simple implementation).
 * Returns an array of pattern strings.
 */
function loadGitignore(dirPath: string): string[] {
  try {
    const gitignorePath = join(dirPath, '.gitignore');
    const content = readFileSync(gitignorePath, 'utf-8');
    return content
      .split('\n')
      .map(line => line.trim())
      .filter(line => line && !line.startsWith('#') && !line.startsWith('!'));
  } catch {
    return [];
  }
}

// Need to import readFileSync for gitignore loading
import { readFileSync } from 'fs';

export const treeTool: Tool = {
  name: 'tree',
  description: 'Display directory structure in a tree-like format. Shows files and directories recursively.',
  
  parameters: {
    type: 'object',
    properties: {
      path: {
        type: 'string',
        description: 'Relative path from project root. Defaults to root. Cannot traverse above project root.',
        default: '.'
      },
      depth: {
        type: 'number',
        description: `Maximum depth to traverse. Default: ${DEFAULT_DEPTH}, Max: ${MAX_DEPTH}`,
        default: DEFAULT_DEPTH
      },
      showHidden: {
        type: 'boolean',
        description: 'Show hidden files (starting with .). Default: false',
        default: false
      },
      respectGitignore: {
        type: 'boolean',
        description: 'Respect .gitignore patterns. Default: true',
        default: true
      }
    },
    required: []
  },

  async execute(params: TreeOptions, context: ToolContext): Promise<string> {
    const { 
      path = '.', 
      depth = DEFAULT_DEPTH, 
      showHidden = false, 
      respectGitignore = true 
    } = params;

    // Jail root is the project root from context
    const jailRoot = resolve(context.projectRoot);
    
    // Resolve the requested path relative to jail root
    const targetPath = resolve(join(jailRoot, path));
    
    // Jail check: ensure target is within jail root
    if (!targetPath.startsWith(jailRoot)) {
      return `Error: Path "${path}" is outside the project root. Access denied.`;
    }

    // Validate depth
    const maxDepth = Math.min(Math.max(1, depth), MAX_DEPTH);

    // Check if target exists and is a directory
    try {
      const stats = statSync(targetPath);
      if (!stats.isDirectory()) {
        return `Error: "${path}" is not a directory.`;
      }
    } catch {
      return `Error: Path "${path}" does not exist.`;
    }

    // Load gitignore patterns if requested
    let gitignorePatterns: string[] = [];
    if (respectGitignore) {
      gitignorePatterns = loadGitignore(jailRoot);
    }

    // Build the tree
    const relativePath = relative(jailRoot, targetPath) || '.';
    let result = `${relativePath}/\n`;
    result += buildTree(
      targetPath,
      jailRoot,
      1,
      maxDepth,
      '',
      showHidden,
      gitignorePatterns
    );

    return result;
  }
};