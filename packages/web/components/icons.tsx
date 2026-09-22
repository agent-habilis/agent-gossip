import type { SVGProps } from 'react'

// Inline rather than lucide-react: one icon is not worth a dependency.
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

export const PlayIcon = (props: SVGProps<SVGSVGElement>) => (
  <Icon width="22" height="22" fill="currentColor" stroke="none" {...props}>
    <path d="M8 5.5v13l11-6.5z" />
  </Icon>
)
