import nextra from 'nextra'

const withNextra = nextra({})

/** @type {import('next').NextConfig} */
const config = {
  reactStrictMode: true,
}

export default withNextra(config)
