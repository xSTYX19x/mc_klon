#[cfg(test)]
mod tests {
    use crate::meshing::pool::MeshingPool;

    #[test]
    fn test_pool_basic_functionality() {
        let mut pool = MeshingPool::new();

        // Test initial state
        let stats = pool.allocation_stats();
        assert!(
            stats.vertices_capacity >= 8192,
            "Should have minimum vertex capacity"
        );
        assert!(
            stats.indices_capacity >= 12288,
            "Should have minimum index capacity"
        );

        // Test reset functionality
        pool.vertices.push(crate::meshing::Vertex {
            position: [1.0, 2.0, 3.0],
            uv: [0.0, 1.0],
            tex_layer: 0,
            color: [1.0, 1.0, 1.0],
            tint_kind: 0,
            light_packed: 0xFF,
        });
        pool.indices.extend_from_slice(&[0, 1, 2]);

        let capacity_before_reset = pool.vertices.capacity();
        pool.reset();

        assert_eq!(pool.vertices.len(), 0, "Vertices should be cleared");
        assert_eq!(pool.indices.len(), 0, "Indices should be cleared");
        assert_eq!(
            pool.vertices.capacity(),
            capacity_before_reset,
            "Capacity should be preserved"
        );
    }

    #[test]
    fn test_pool_take_mesh_data() {
        let mut pool = MeshingPool::new();

        pool.vertices.push(crate::meshing::Vertex {
            position: [1.0, 2.0, 3.0],
            uv: [0.5, 0.5],
            tex_layer: 1,
            color: [0.8, 0.8, 0.8],
            tint_kind: 0,
            light_packed: 0xFF,
        });
        pool.indices.extend_from_slice(&[0, 1, 2]);

        let mesh = pool.take_mesh_data();
        assert_eq!(mesh.vertices.len(), 1, "Mesh should contain the vertex");
        assert_eq!(mesh.indices.len(), 3, "Mesh should contain the indices");
        assert_eq!(
            mesh.vertices[0].position,
            [1.0, 2.0, 3.0],
            "Vertex data should be preserved"
        );

        // Pool should be empty but retain capacity
        assert_eq!(
            pool.vertices.len(),
            0,
            "Pool vertices should be empty after take"
        );
        assert_eq!(
            pool.indices.len(),
            0,
            "Pool indices should be empty after take"
        );
        assert!(
            pool.vertices.capacity() >= 8192,
            "Pool should retain minimum capacity"
        );
        assert!(
            pool.indices.capacity() >= 12288,
            "Pool should retain minimum capacity"
        );
    }

    #[test]
    fn test_thread_local_pool_access() {
        // Test that thread-local pool can be accessed and used
        super::super::pool::with_meshing_pool(|pool| {
            pool.vertices.push(crate::meshing::Vertex {
                position: [0.0, 0.0, 0.0],
                uv: [0.0, 0.0],
                tex_layer: 0,
                color: [1.0, 1.0, 1.0],
                tint_kind: 0,
                light_packed: 0,
            });

            assert_eq!(
                pool.vertices.len(),
                1,
                "Should be able to add vertices to thread-local pool"
            );
            pool.vertices.len() // Return something to test the closure
        });

        // Verify pool is reset between uses
        super::super::pool::with_meshing_pool(|pool| {
            assert_eq!(
                pool.vertices.len(),
                0,
                "Thread-local pool should be reset between uses"
            );
        });
    }
}
