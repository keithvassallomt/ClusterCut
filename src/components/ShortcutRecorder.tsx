import { useState, useEffect, useRef } from 'react';
import { X, Check, Keyboard, Edit2 } from 'lucide-react';


interface ShortcutRecorderProps {
  value: string | null;
  onChange: (value: string | null) => void;
  placeholder?: string;
}

export function ShortcutRecorder({ value, onChange, placeholder = "Record Shortcut" }: ShortcutRecorderProps) {
  const [isRecording, setIsRecording] = useState(false);
  const [currentCombo, setCurrentCombo] = useState<string[]>([]);
  const inputRef = useRef<HTMLDivElement>(null);

  // Detect Platform for Display
  const isMac = navigator.userAgent.toLowerCase().includes('mac');

  // Parse existing value for display
  const displayParts = value 
    ? value.split('+').map(part => {
        if (part === 'CommandOrControl') return isMac ? 'Cmd' : 'Ctrl';
        if (part === 'Command') return 'Cmd';
        if (part === 'Control') return 'Ctrl';
        return part;
      })
    : [];

  useEffect(() => {
    if (!isRecording) return;

    const handleKeyDown = (e: KeyboardEvent) => {
      e.preventDefault();
      
      const parts: string[] = [];
      
      // Tauri Global Shortcut Format Mapping
      // Modifiers
      if (e.metaKey) parts.push('Command'); // Or CommandOrControl? 
      // Better to map Meta/Control based on platform if we want "CommandOrControl"?
      // If we want universal, we should always produce 'CommandOrControl' if it's the primary modifier.
      // Setup:
      // Mac: Cmd -> CommandOrControl
      // Win/Lin: Ctrl -> CommandOrControl
      
      // If we just use standard names:
      // "Super" / "Meta" -> Command
      // "Control" -> Control
      
      // Logic: Capture physical keys.
      // If we want to support "CommandOrControl" (Tauri abstraction):
      // On Mac: e.metaKey is true. We push 'CommandOrControl'.
      // On Win: e.ctrlKey is true. We push 'CommandOrControl'.
      // But wait, what if user wants actual Control on Mac?
      // Usually "CommandOrControl" is the intent for "Primary Modifier".
      
      if (isMac) {
          if (e.metaKey) parts.push('CommandOrControl');
          if (e.ctrlKey) parts.push('Control');
          if (e.altKey) parts.push('Alt');
          if (e.shiftKey) parts.push('Shift');
      } else {
          if (e.ctrlKey) parts.push('CommandOrControl');
          // Meta on windows is usually Windows Key -> 'Super'? Tauri uses 'Super'.
          if (e.metaKey) parts.push('Super');
          if (e.altKey) parts.push('Alt');
          if (e.shiftKey) parts.push('Shift');
      }

      // Key
      // Ignore modifier keys themselves as "The Key"
      const key = e.key.toUpperCase();
      if (!['CONTROL', 'META', 'ALT', 'SHIFT', 'OS', 'COMMAND'].includes(key)) {
          // Map special keys?
          // e.g. " " -> "Space"
          if (key === ' ') parts.push('Space');
          else if (key.length === 1) parts.push(key);
          else {
              // Function keys, arrow keys etc.
              // Tauri uses 'F1', 'Enter', 'Space', 'Delete', 'ArrowUp', etc.
              // e.key usually matches well, might need some normalization.
              // For main letter keys, we want UpperCase.
              // For others, keep TitleCase? e.g. "ArrowUp"
              // e.key is "ArrowUp".
              // e.key is "Enter".
              // e.key is "Backspace".
              if (key === 'ESCAPE') {
                 // Cancel recording?
                 setIsRecording(false);
                 return;
              }
              parts.push(e.key);
          }
      }
      
      setCurrentCombo(parts);
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [isRecording, isMac]);

  const save = () => {
    if (currentCombo.length > 0) {
      onChange(currentCombo.join('+'));
    }
    setIsRecording(false);
  };

  const cancel = () => {
    setIsRecording(false);
    setCurrentCombo([]);
  };

  const clear = () => {
     onChange(null);
  };

  if (isRecording) {
    return (
      <div 
        ref={inputRef}
        className="flex items-center justify-between rounded-lg border-2 border-emerald-500/50 bg-emerald-500/5 p-2"
      >
        <div className="flex flex-wrap items-center gap-1">
          {currentCombo.length > 0 ? (
            currentCombo.map((k, i) => (
              <Kbd key={i} label={k === 'CommandOrControl' ? (isMac ? '⌘' : 'Ctrl') : k} />
            ))
          ) : (
            <span className="text-sm italic text-zinc-500">Press keys...</span>
          )}
        </div>
        
        <div className="flex gap-1">
           <button onClick={cancel} className="rounded p-1 text-zinc-500 hover:bg-zinc-200 dark:hover:bg-zinc-700">
               <X className="h-4 w-4" />
           </button>
           <button onClick={save} className="rounded p-1 text-emerald-600 hover:bg-emerald-100 dark:text-emerald-400 dark:hover:bg-emerald-900/30">
               <Check className="h-4 w-4" />
           </button>
        </div>
      </div>
    );
  }

  return (
    <div className="flex items-center justify-between rounded-lg border border-zinc-200 bg-white p-2 dark:border-white/10 dark:bg-zinc-900">
       <div className="flex flex-wrap items-center gap-1">
          {displayParts.length > 0 ? (
              displayParts.map((k, i) => <Kbd key={i} label={k.replace('CommandOrControl', isMac ? 'Cmd' : 'Ctrl')} />)
          ) : (
              <span className="text-sm text-zinc-400">{placeholder}</span>
          )}
       </div>
       <div className="flex gap-1">
          {value && (
               <button onClick={clear} className="rounded p-1 text-zinc-400 hover:bg-zinc-100 dark:hover:bg-zinc-800" title="Clear">
                   <TrashIcon />
               </button>
          )}
          <button 
            onClick={() => { setIsRecording(true); setCurrentCombo([]); }} 
            className="flex items-center gap-1.5 rounded bg-zinc-100 px-2 py-1 text-xs font-medium text-zinc-700 hover:bg-zinc-200 dark:bg-zinc-800 dark:text-zinc-300 dark:hover:bg-zinc-700"
          >
             {value ? <Edit2 className="h-3 w-3" /> : <Keyboard className="h-3 w-3" />}
             {value ? "Change" : "Record"}
          </button>
       </div>
    </div>
  );
}

export function Kbd({ label }: { label: string }) {
    return (
        <div className="min-w-[20px] rounded bg-zinc-100 px-1.5 py-0.5 text-center text-xs font-semibold text-zinc-600 shadow-sm ring-1 ring-inset ring-zinc-300 dark:bg-zinc-800 dark:text-zinc-300 dark:ring-zinc-700">
            {label}
        </div>
    );
}

function TrashIcon() {
    return (
        <svg xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <path d="M3 6h18"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6m3 0V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/>
        </svg>
    )
}
