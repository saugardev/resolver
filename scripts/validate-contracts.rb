#!/usr/bin/env ruby
# frozen_string_literal: true

require "json"
require "yaml"

root = File.expand_path("..", __dir__)
openapi = YAML.safe_load(
  File.read(File.join(root, "docs/openapi.yaml")),
  aliases: false
)

raise "docs/openapi.yaml must use OpenAPI 3.1.0" unless openapi["openapi"] == "3.1.0"

required_paths = %w[
  /
  /.well-known/oauth-protected-resource
  /.well-known/oauth-protected-resource/mcp
  /crawl
  /extract
  /fetch
  /fetchfast
  /fetchunblock
  /healthz
  /map
  /mcp
  /metrics
  /readyz
  /receipt/{id}
  /screenshot
  /search
  /snapshot
]
missing_paths = required_paths - openapi.fetch("paths").keys
raise "OpenAPI is missing paths: #{missing_paths.join(", ")}" unless missing_paths.empty?

references = []
walk = lambda do |value|
  case value
  when Hash
    references << value["$ref"] if value.key?("$ref")
    value.each_value { |nested| walk.call(nested) }
  when Array
    value.each { |nested| walk.call(nested) }
  end
end
walk.call(openapi)

references.uniq.each do |reference|
  raise "external OpenAPI reference is not pinned: #{reference}" unless reference.start_with?("#/")

  reference.delete_prefix("#/").split("/").reduce(openapi) do |node, token|
    key = token.gsub("~1", "/").gsub("~0", "~")
    raise "missing OpenAPI reference: #{reference}" unless node.is_a?(Hash) && node.key?(key)

    node[key]
  end
end

operation_ids = []
openapi.fetch("paths").each_value do |path|
  path.each_value do |operation|
    if operation.is_a?(Hash) && operation.key?("operationId")
      operation_ids << operation["operationId"]
    end
  end
end
raise "OpenAPI operationId values must be unique" unless operation_ids.uniq.length == operation_ids.length

mcp = JSON.parse(File.read(File.join(root, "docs/mcp-client.example.json")))
resolver = mcp.fetch("mcpServers").fetch("livy-resolver")
raise "MCP example must use Streamable HTTP" unless resolver["type"] == "http"
raise "MCP example must target /mcp" unless resolver["url"] == "http://localhost:3001/mcp"
