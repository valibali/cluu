/*
 * IPC Syscall Interface
 *
 * C header providing IPC syscall function prototypes for CLUU userspace programs.
 * These wrappers provide access to the kernel's port-based IPC system.
 */

#ifndef IPC_H
#define IPC_H

/* Port ID type */
typedef long port_id_t;

/* IPC Message (256 bytes) */
#define IPC_MSG_SIZE 256

struct ipc_message {
    unsigned char data[IPC_MSG_SIZE];
};

/* IPC error codes (negative return values) */
#define IPC_ERR_INVALID     -22   /* Invalid argument */
#define IPC_ERR_NO_MSG      -42   /* No message available */
#define IPC_ERR_QUEUE_FULL  -11   /* Queue full, cannot send */
#define IPC_ERR_NOT_FOUND   -2    /* Port not found */

/* IPC Syscall Wrappers */

/**
 * Create a new IPC port
 * Returns: port ID on success, or negative error code
 */
port_id_t port_create(void);

/**
 * Destroy an IPC port
 * Returns: 0 on success, or negative error code
 */
int port_destroy(port_id_t port);

/**
 * Send a message to a port (non-blocking)
 * Returns: 0 on success, or negative error code
 */
int port_send(port_id_t port, const struct ipc_message *msg);

/**
 * Receive a message from a port (blocking)
 * Returns: 0 on success, or negative error code
 */
int port_recv(port_id_t port, struct ipc_message *msg);

/**
 * Try to receive a message from a port (non-blocking)
 * Returns: 0 on success, IPC_ERR_NO_MSG if no message, or negative error code
 */
int port_try_recv(port_id_t port, struct ipc_message *msg);

/**
 * Register a well-known port name
 * Args:
 *   name: null-terminated string (port name)
 *   port: port ID to register
 * Returns: 0 on success, or negative error code
 */
int register_port_name(const char *name, port_id_t port);

/**
 * Look up a port by well-known name
 * Args:
 *   name: null-terminated string (port name)
 * Returns: port ID on success, or negative error code
 */
port_id_t lookup_port_name(const char *name);

#endif /* IPC_H */
