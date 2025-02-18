#!/usr/bin/env python3
import sys
from collections import defaultdict, deque

def find_reachable_nodes(graph, start_node):
    """Find all nodes reachable from start_node using BFS."""
    reachable = set()
    queue = deque([start_node])
    
    while queue:
        node = queue.popleft()
        if node not in reachable:
            reachable.add(node)
            for neighbor in graph[node]:
                if neighbor not in reachable:
                    queue.append(neighbor)
    
    return reachable

def find_all_paths(G, u, v):
    # Initialize visited array as dictionary for easier node lookup
    visited = {node: False for node in G}
    path_to_target = {}

    current_path = []
    simple_paths = []
    
    def dfs(u, v, visited):
        if visited[u]:
            return
            
        visited[u] = True
        if u not in path_to_target:
            path_to_target[u] = False
        current_path.append(u)
        
        if u == v:
            # Create a copy of current_path to store in simple_paths
            simple_paths.append(current_path[:])
            visited[u] = False
            path_to_target[u] = True
            current_path.pop()
            return
            
        for next_node in G[u]:
            if not(next_node in path_to_target) or path_to_target[next_node]:
                dfs(next_node, v, visited)
            path_to_target[u] = (u in path_to_target and path_to_target[u]) or path_to_target[next_node]
            
        current_path.pop()
        visited[u] = False
    
    dfs(u, v, visited)
    return simple_paths


def parse_dot_edges(input_lines):
    """Parse edge lines into an adjacency list."""
    graph = defaultdict(list)
    for line in input_lines:
        if '->' in line:
            line = line.replace('"', '').replace(';', '').strip()
            source, target = line.split('->')
            source = source.strip()
            target = target.strip()
            graph[source].append(target)
    return graph

def add_namespace_edges(edges):
    """Add edges between namespace parents and children."""
    additional_edges = set()
    nodes = set()
    
    for line in edges:
        if '->' in line:
            source, target = line.split('->')
            source = source.strip().replace('"', '')
            target = target.strip().replace('"', '').replace(';', '')
            nodes.add(source)
            nodes.add(target)
    
    for node in nodes:
        if '::' in node:
            parts = node.split('::')
            for i in range(len(parts) - 1):
                parent = '::'.join(parts[:i+1])
                child = '::'.join(parts[:i+2])
                additional_edges.add(f'"{parent}" -> "{child}"')
    
    return edges + list(additional_edges)

def filter_and_highlight_paths(input_lines, start_node, target_node):
    """Filter graph to reachable nodes and highlight paths from start to target."""
    # Add namespace hierarchy edges
    input_lines = add_namespace_edges(input_lines)
    
    # Parse the graph
    graph = parse_dot_edges(input_lines)
    
    # Find reachable nodes from start_node
    reachable = find_reachable_nodes(graph, start_node)
    
    # Create filtered graph containing only reachable nodes
    filtered_graph = defaultdict(list)
    for source, targets in graph.items():
        if source in reachable:
            filtered_graph[source] = [t for t in targets if t in reachable]
    
    # Find all paths from start to target in filtered graph
    paths = find_all_paths(filtered_graph, start_node, target_node)
    
    if paths:
        print(f"Found {len(paths)} paths from {start_node} to {target_node}:", file=sys.stderr)
        for i, path in enumerate(paths, 1):
            print(f"Path {i}: {' -> '.join(path)}", file=sys.stderr)
    else:
        print(f"No paths found from {start_node} to {target_node}", file=sys.stderr)
    
    # Collect all nodes and edges in the paths
    path_nodes = set()
    path_edges = set()
    for path in paths:
        path_nodes.update(path)
        for i in range(len(path) - 1):
            path_edges.add((path[i], path[i + 1]))
    
    # Create DOT output
    dot_output = ['digraph G {']
    dot_output.extend([
        '    layout=dot;',
        '    rankdir=LR;',
        '    concentrate=true;',
        '    node [shape=box, style="rounded,filled", fillcolor="#f0f0f0"];',
        '    edge [color="#666666"];'
    ])
    
    # Add nodes with special styling
    dot_output.append(f'    "{start_node}" [fillcolor="#90EE90"];')  # Start node in green
    if target_node in reachable:
        dot_output.append(f'    "{target_node}" [fillcolor="#FFB6C6"];')  # Target node in pink
    
    # Add path nodes with special styling
    for node in path_nodes:
        if node != start_node and node != target_node:
            dot_output.append(f'    "{node}" [fillcolor="#ADD8E6"];')  # Path nodes in light blue
    
    # Add edges (only for reachable nodes)
    for line in input_lines:
        if '->' in line:
            line = line.replace('"', '').replace(';', '').strip()
            source, target = line.split('->')
            source = source.strip()
            target = target.strip()
            
            if source in reachable and target in reachable:
                if (source, target) in path_edges:
                    # Edge is part of a path
                    dot_output.append(f'    "{source}" -> "{target}" [color="#0000FF", penwidth=2.0];')
                else:
                    dot_output.append(f'    "{source}" -> "{target}";')
    
    dot_output.append('}')
    return '\n'.join(dot_output)

def main():
    if len(sys.argv) != 4:
        print("Usage: python script.py <input_file> <start_node> <target_node>")
        sys.exit(1)
    
    input_file = sys.argv[1]
    start_node = sys.argv[2]
    target_node = sys.argv[3]
    
    try:
        with open(input_file, 'r') as f:
            input_lines = [line.strip() for line in f if '->' in line]
    except FileNotFoundError:
        print(f"Error: File '{input_file}' not found")
        sys.exit(1)
    except IOError as e:
        print(f"Error reading file: {e}")
        sys.exit(1)
    
    highlighted_graph = filter_and_highlight_paths(input_lines, start_node, target_node)
    print(highlighted_graph)

if __name__ == "__main__":
    main()