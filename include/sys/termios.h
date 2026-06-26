#ifndef _SYS_TERMIOS_H
#define _SYS_TERMIOS_H

#include <sys/types.h>

typedef unsigned int tcflag_t;
typedef unsigned char cc_t;
typedef unsigned int speed_t;

#define NCCS 20

struct termios {
    tcflag_t c_iflag;   /* input modes */
    tcflag_t c_oflag;   /* output modes */
    tcflag_t c_cflag;   /* control modes */
    tcflag_t c_lflag;   /* local modes */
    cc_t     c_cc[NCCS]; /* control characters */
    speed_t  c_ispeed;  /* input speed */
    speed_t  c_ospeed;  /* output speed */
};

/* c_lflag bits — must match cluu_wire::pts::Termios */
#define ISIG    0x0001
#define ICANON  0x0002
#define ECHO    0x0004
#define ECHOE   0x0008
#define ECHOK   0x0010
#define ECHONL  0x0020
#define NOFLSH  0x0040
#define TOSTOP  0x0080
#define ECHOCTL 0x0100
#define ECHOPRT 0x0200
#define ECHOKE  0x0400
#define IEXTEN  0x0800

/* c_iflag bits — must match cluu_wire::pts::Termios */
#define IGNBRK  0x0001
#define BRKINT  0x0002
#define ICRNL   0x0004
#define INLCR   0x0008
#define IXON    0x0010
#define IXOFF   0x0020

/* c_oflag bits — must match cluu_wire::pts::Termios */
#define OPOST   0x0001
#define ONLCR   0x0002

/* c_cflag bits — must match cluu_wire::pts::Termios */
#define CREAD   0x0001
#define HUPCL   0x0002
#define CLOCAL  0x0004

/* c_cc indices — must match cluu_wire::pts::Termios */
#define VEOF     0
#define VEOL     1
#define VERASE   2
#define VINTR    3
#define VKILL    4
#define VMIN     5
#define VQUIT    6
#define VSTART   7
#define VSTOP    8
#define VSUSP    9
#define VTIME    10
#define VWERASE  11

/* tcsetattr actions */
#define TCSANOW   0
#define TCSADRAIN 1
#define TCSAFLUSH 2

int tcgetattr(int fd, struct termios *termios_p);
int tcsetattr(int fd, int optional_actions, const struct termios *termios_p);

#endif /* _SYS_TERMIOS_H */
