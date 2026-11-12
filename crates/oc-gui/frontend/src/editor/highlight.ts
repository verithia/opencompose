/**
 * Simple syntax highlighter for OCP DSL.
 * Returns HTML with <span> tags for colored tokens.
 *
 * Uses the same token classes as the TextMate grammar so CSS is shared
 * between this inline highlighter and a future CodeMirror/VS Code integration.
 */

interface Rule {
  pattern: RegExp;
  className: string;
}

const RULES: Rule[] = [
  // Comments (must be first)
  { pattern: /\/\/.*$/gm, className: "syn-comment" },
  // Strings
  { pattern: /"[^"]*"/g, className: "syn-string" },
  // Keywords (@project, @track, etc.)
  { pattern: /@(?:project|track|phrase|pattern|bus|master|section)\b/g, className: "syn-keyword" },
  // Properties (tempo:, instrument:, etc.)
  { pattern: /\b(?:tempo|time_signature|key|swing|instrument|engine|mix|effects|routes|automation|loudness|repeat|label|engine_params):/g, className: "syn-property" },
  // Automation attributes (curve=stairs, smoothing=25ms, tension=0.4)
  { pattern: /\b(?:curve|tension|smoothing)=/g, className: "syn-property" },
  // Pitches (C4, Bb3, F#5)
  { pattern: /\b[A-G](?:##?|bb?)?[0-9]\b/g, className: "syn-pitch" },
  // Durations (q, h., 8th, 16th~q)
  { pattern: /\b(?:w|h|q|8th|16th|32nd|64th)(?:\.{1,2})?(?:t)?(?:~(?:w|h|q|8th|16th|32nd|64th))?\b/g, className: "syn-duration" },
  // Dynamics
  { pattern: /\b(?:ppp|pp|mp|mf|fff|ff|p(?!a)|f(?!l))\b/g, className: "syn-dynamics" },
  // Articulations
  { pattern: /\b(?:legato|staccato|staccatissimo|tenuto|marcato|accent|pizzicato|arco|tremolo|trill|col_legno|harmonics|sul_tasto|sul_ponticello|muted|open|flutter_tongue|sforzando|palm_mute|hammer_on|pull_off|slide|bend|ghost_note|flam|roll|rim_shot|sustained|detached)\b/g, className: "syn-articulation" },
  // Drums
  { pattern: /\b(?:kick|snare|hihat|crash|ride|splash|china|tom_high|tom_mid|tom_low|cowbell|clap|rim|shaker|tambourine)\b/g, className: "syn-drum" },
  // Numbers with units
  { pattern: /[+-]?\d+(?:\.\d+)?(?:dB|%|Hz|ms|s|ct)\b/g, className: "syn-number-unit" },
  // Modifiers
  { pattern: /\b(?:vibrato|expression|articulation|send|vol|pan|mute|solo)\b/g, className: "syn-modifier" },
  // Bar line
  { pattern: /^\s*\|\s*$/gm, className: "syn-barline" },
  // Rest
  { pattern: /\brest\b/g, className: "syn-rest" },
];

/**
 * Highlight OCP source code and return HTML string.
 */
export function highlightOcp(source: string): string {
  // Tokenize: collect all matches with positions, sort by position,
  // then build HTML by interleaving plain text and highlighted spans.
  interface Token {
    start: number;
    end: number;
    className: string;
    text: string;
  }

  const tokens: Token[] = [];

  for (const rule of RULES) {
    // Reset regex state
    rule.pattern.lastIndex = 0;
    let match;
    while ((match = rule.pattern.exec(source)) !== null) {
      tokens.push({
        start: match.index,
        end: match.index + match[0].length,
        className: rule.className,
        text: match[0],
      });
    }
  }

  // Sort by start position, longer matches first for ties
  tokens.sort((a, b) => a.start - b.start || b.end - a.end);

  // Remove overlapping tokens (keep first match)
  const filtered: Token[] = [];
  let lastEnd = 0;
  for (const t of tokens) {
    if (t.start >= lastEnd) {
      filtered.push(t);
      lastEnd = t.end;
    }
  }

  // Build HTML
  const parts: string[] = [];
  let pos = 0;
  for (const t of filtered) {
    if (t.start > pos) {
      parts.push(escapeHtml(source.slice(pos, t.start)));
    }
    parts.push(`<span class="${t.className}">${escapeHtml(t.text)}</span>`);
    pos = t.end;
  }
  if (pos < source.length) {
    parts.push(escapeHtml(source.slice(pos)));
  }

  return parts.join("");
}

function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}
