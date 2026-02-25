# Scry MCP

scry-mcp is an MCP built in rust with two tools: `whiteboard()` and `whiteboard_list()`.
A whiteboard call includes python code for generating SVG.

The pyO3 python interpreter has advisory sandboxing to prevent mistakes, but is not intended
as a security measure. Use virtual machines or containers to help with that.

