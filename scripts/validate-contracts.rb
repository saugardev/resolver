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

fetch_responses = openapi.fetch("paths").fetch("/fetch").fetch("post").fetch("responses")
expected_fetch_responses = {
  "200" => "#/components/responses/ProductSuccess",
  "400" => "#/components/responses/BadRequest",
  "401" => "#/components/responses/Unauthorized",
  "402" => "#/components/responses/PaymentRequired",
  "403" => "#/components/responses/Forbidden",
  "409" => "#/components/responses/Conflict",
  "413" => "#/components/responses/PayloadTooLarge",
  "415" => "#/components/responses/UnsupportedMediaType",
  "500" => "#/components/responses/InternalError",
  "502" => "#/components/responses/UpstreamFailure",
  "503" => "#/components/responses/Unavailable",
  "504" => "#/components/responses/Timeout"
}
unless fetch_responses.keys.sort == expected_fetch_responses.keys.sort
  raise "POST /fetch must document the exact application status policy (no default)"
end
expected_fetch_responses.each do |status, reference|
  unless fetch_responses.fetch(status)["$ref"] == reference
    raise "POST /fetch response #{status} must reference #{reference}"
  end
end

receipt_parameters = openapi
  .fetch("paths")
  .fetch("/receipt/{id}")
  .fetch("get")
  .fetch("parameters", [])
receipt_has_idempotency_key = receipt_parameters.any? do |parameter|
  parameter["$ref"] == "#/components/parameters/IdempotencyKey" ||
    (parameter["in"] == "header" && parameter["name"].to_s.casecmp("Idempotency-Key").zero?)
end
raise "GET /receipt/{id} must not advertise Idempotency-Key" if receipt_has_idempotency_key

expected_mcp_responses = {
  "401" => "#/components/responses/Unauthorized",
  "403" => "#/components/responses/McpForbidden",
  "413" => "#/components/responses/McpPayloadTooLarge",
  "500" => "#/components/responses/McpInternalError",
  "503" => "#/components/responses/Unavailable",
  "504" => "#/components/responses/Timeout",
  "default" => "#/components/responses/McpTransportResponse"
}
%w[/ /mcp].each do |path|
  responses = openapi.fetch("paths").fetch(path).fetch("post").fetch("responses")
  unless responses.key?("200") && (expected_mcp_responses.keys - responses.keys).empty?
    raise "POST #{path} must cover the audited MCP HTTP response policy"
  end
  expected_mcp_responses.each do |status, reference|
    unless responses.fetch(status)["$ref"] == reference
      raise "POST #{path} response #{status} must reference #{reference}"
    end
  end
end

%w[McpForbidden McpInternalError McpTransportResponse].each do |name|
  response = openapi.fetch("components").fetch("responses").fetch(name)
  raise "#{name} must not assert one response body shape" if response.key?("content")
end
mcp_payload_content = openapi
  .fetch("components")
  .fetch("responses")
  .fetch("McpPayloadTooLarge")
  .fetch("content")
raise "MCP 413 must document its plain-text middleware body" unless mcp_payload_content.keys == ["text/plain"]

http_methods = %w[get post put patch delete options head trace]
global_security = openapi.fetch("security", [])
openapi.fetch("paths").each do |path, path_item|
  path_item.each do |method, operation|
    next unless http_methods.include?(method) && operation.is_a?(Hash)

    security = operation.fetch("security", global_security)
    next if security.nil? || security.empty?

    responses = operation.fetch("responses")
    next if responses.key?("default")
    next if path == "/fetch" && method == "post"

    raise "authenticated operation #{method.upcase} #{path} needs a safe default response"
  end
end

mcp = JSON.parse(File.read(File.join(root, "docs/mcp-client.example.json")))
resolver = mcp.fetch("mcpServers").fetch("livy-resolver")
raise "MCP example must use Streamable HTTP" unless resolver["type"] == "http"
raise "MCP example must target /mcp" unless resolver["url"] == "http://localhost:3001/mcp"
