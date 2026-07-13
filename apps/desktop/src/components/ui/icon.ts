// Central icon module (#284 §1). The single place the app pulls `lucide-react`
// icons from — a `no-restricted-imports` ESLint rule forbids importing
// `lucide-react` anywhere else, so this is the one chokepoint to standardize the
// icon set, restyle/theme, or swap the library later.
//
// Usage: `import { Check, Copy } from "@/components/ui/icon"`. Icons are plain
// lucide components — pass `className` for sizing (the app convention is
// `size-3.5` for inline UI, `size-3` for compact contexts) and color
// (`text-*`); lucide's default `strokeWidth` (2) is used throughout.
//
// This is a curated re-export of exactly the icons used in the app. When a
// component needs a new icon, add it here (kept alphabetical).

export {
  AlertTriangle,
  ArrowUp,
  Blocks,
  Brain,
  CalendarClock,
  Check,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  ChevronUp,
  ChevronsUpDown,
  CircleDot,
  CircleOff,
  CirclePlay,
  Columns2,
  Copy,
  CornerDownLeft,
  Cpu,
  Dna,
  Download,
  EyeOff,
  FileText,
  FlaskConical,
  Folder,
  FolderPlus,
  Gauge,
  GitBranch,
  // Aliased: a bare `Image` re-export would shadow the global DOM `Image`
  // constructor in any consumer (#284 §1 review nit).
  Image as ImageIcon,
  Info,
  Keyboard,
  Layers,
  Loader2,
  Lock,
  MessageCircleQuestion,
  MessageSquare,
  Moon,
  MoreHorizontal,
  Package,
  Palette,
  PanelLeft,
  PanelRight,
  Paperclip,
  Pause,
  Pencil,
  PencilLine,
  Pin,
  PinOff,
  Play,
  PlugZap,
  Plus,
  Power,
  RotateCcw,
  Rows2,
  Search,
  Server,
  Settings,
  ShieldAlert,
  ShieldCheck,
  SlidersHorizontal,
  Sparkles,
  SplitSquareHorizontal,
  SplitSquareVertical,
  Square,
  SquareArrowOutUpRight,
  SquareCheck,
  Star,
  Sun,
  Target,
  TextCursorInput,
  Trash2,
  Upload,
  Users,
  WrapText,
  X,
} from "lucide-react";

export type { LucideIcon } from "lucide-react";
