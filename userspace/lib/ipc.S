/*
 * IPC Syscall Wrappers (Assembly)
 *
 * Low-level syscall wrappers for IPC operations.
 * Uses the SYSCALL instruction to enter kernel mode.
 */

.section .text

/* Syscall numbers */
.set SYS_PORT_CREATE,      1000
.set SYS_PORT_DESTROY,     1001
.set SYS_PORT_SEND,        1002
.set SYS_PORT_RECV,        1003
.set SYS_PORT_TRY_RECV,    1004
.set SYS_REGISTER_PORT_NAME, 1005
.set SYS_LOOKUP_PORT_NAME, 1006

/* port_id_t port_create(void) */
.globl port_create
.type port_create, @function
port_create:
    mov $SYS_PORT_CREATE, %rax
    syscall
    ret

/* int port_destroy(port_id_t port) */
.globl port_destroy
.type port_destroy, @function
port_destroy:
    /* port is already in %rdi (first argument) */
    mov $SYS_PORT_DESTROY, %rax
    syscall
    ret

/* int port_send(port_id_t port, const struct ipc_message *msg) */
.globl port_send
.type port_send, @function
port_send:
    /* port in %rdi, msg in %rsi */
    mov $SYS_PORT_SEND, %rax
    syscall
    ret

/* int port_recv(port_id_t port, struct ipc_message *msg) */
.globl port_recv
.type port_recv, @function
port_recv:
    /* port in %rdi, msg in %rsi */
    mov $SYS_PORT_RECV, %rax
    syscall
    ret

/* int port_try_recv(port_id_t port, struct ipc_message *msg) */
.globl port_try_recv
.type port_try_recv, @function
port_try_recv:
    /* port in %rdi, msg in %rsi */
    mov $SYS_PORT_TRY_RECV, %rax
    syscall
    ret

/* int register_port_name(const char *name, port_id_t port) */
.globl register_port_name
.type register_port_name, @function
register_port_name:
    /* name in %rdi, port in %rsi */
    mov $SYS_REGISTER_PORT_NAME, %rax
    syscall
    ret

/* port_id_t lookup_port_name(const char *name) */
.globl lookup_port_name
.type lookup_port_name, @function
lookup_port_name:
    /* name in %rdi */
    mov $SYS_LOOKUP_PORT_NAME, %rax
    syscall
    ret
