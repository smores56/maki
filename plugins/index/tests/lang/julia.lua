local helpers = require("tests.helpers")
local case = helpers.case
local idx = helpers.idx
local has = helpers.has
local lacks = helpers.lacks

case("julia_query_driver_emits_definitions", function()
  local src = [[
const MAX = 10

function add(a, b)
  return a + b
end

struct Point
  x::Int
  y::Int
end

abstract type Shape end

module Geometry
using Base
import LinearAlgebra: dot
export area

const PI = 3.14

function area(s::Shape)
  0
end

macro warn(expr)
  expr
end
end
]]
  local out = idx(src, "julia")

  lacks(out, { "return a + b" })
  has(out, {
    "consts:",
    "fns:",
    "types:",
    "mod:",
    "imports:",
  })
  has(out, {
    "MAX = 10",
    "add(a, b)",
    "Point",
    "Shape",
    "Geometry",
    "PI = 3.14",
    "area(s::Shape)",
    "warn(expr)",
  })
end)

case("julia_query_driver_empty_and_trivial", function()
  local out = idx("", "julia")
  has(out, {})
  lacks(out, { "fns:", "consts:", "types:" })

  local trivial = idx("# just a comment\n", "julia")
  has(trivial, {})
  lacks(trivial, { "fns:", "types:", "consts:" })
end)

case("julia_query_driver_module_inner_definitions_surface", function()
  local src = [[
module Inner
function hidden() end
struct Hidden end
end
]]
  local out = idx(src, "julia")
  has(out, { "mod:", "Inner", "hidden()", "Hidden" })
end)
