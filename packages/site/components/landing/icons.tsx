import type { SVGProps } from 'react'

// Inline rather than lucide-react: bun installed lucide isolated under
// fumadocs-ui, so this package cannot resolve it, and six icons are not worth a
// dependency.
function Icon({ children, ...props }: SVGProps<SVGSVGElement>) {
  return (
    <svg
      viewBox="0 0 24 24"
      width="20"
      height="20"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.5"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      {...props}
    >
      {children}
    </svg>
  )
}

export const CopyIcon = (props: SVGProps<SVGSVGElement>) => (
  <Icon width="16" height="16" {...props}>
    <rect x="9" y="9" width="11" height="11" rx="2" />
    <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
  </Icon>
)

export const CheckIcon = (props: SVGProps<SVGSVGElement>) => (
  <Icon width="16" height="16" {...props}>
    <path d="m4 12 5 5L20 6" />
  </Icon>
)

export const PlayIcon = (props: SVGProps<SVGSVGElement>) => (
  <Icon width="22" height="22" fill="currentColor" stroke="none" {...props}>
    <path d="M8 5.5v13l11-6.5z" />
  </Icon>
)

export const HarnessIcon = (props: SVGProps<SVGSVGElement>) => (
  <Icon {...props}>
    <rect x="2" y="4" width="20" height="16" rx="2" />
    <path d="m6 9 3 3-3 3M12 15h5" />
  </Icon>
)

export const MachinesIcon = (props: SVGProps<SVGSVGElement>) => (
  <Icon {...props}>
    <circle cx="12" cy="12" r="3" />
    <circle cx="4" cy="6" r="2" />
    <circle cx="20" cy="6" r="2" />
    <circle cx="4" cy="18" r="2" />
    <circle cx="20" cy="18" r="2" />
    <path d="m6 7 4 3m8-3-4 3M6 17l4-3m8 3-4-3" />
  </Icon>
)

export const DelegateIcon = (props: SVGProps<SVGSVGElement>) => (
  <Icon {...props}>
    <path d="M4 6h10M4 12h7M4 18h10" />
    <path d="m16 9 3 3-3 3" />
    <path d="M19 12h1" />
  </Icon>
)

export const GateIcon = (props: SVGProps<SVGSVGElement>) => (
  <Icon {...props}>
    <rect x="4" y="10" width="16" height="10" rx="2" />
    <path d="M8 10V7a4 4 0 0 1 8 0v3" />
  </Icon>
)

export const StateIcon = (props: SVGProps<SVGSVGElement>) => (
  <Icon {...props}>
    <ellipse cx="12" cy="6" rx="8" ry="3" />
    <path d="M4 6v12c0 1.7 3.6 3 8 3s8-1.3 8-3V6" />
    <path d="M4 12c0 1.7 3.6 3 8 3s8-1.3 8-3" />
  </Icon>
)

export const ScaleIcon = (props: SVGProps<SVGSVGElement>) => (
  <Icon {...props}>
    <path d="M3 19h18" />
    <path d="M6 19v-5m5 5V9m5 10V6" />
  </Icon>
)
