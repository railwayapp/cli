package random

import (
	cryptoRand "crypto/rand"
	"encoding/base64"
	"fmt"
	"math/rand"
	mathRand "math/rand"
	"net"
	"time"
)

type Randomizer struct {
	mathRand.Rand
}

// Bytes returns securely generated random bytes.
// It will return an error if the system's secure random
// number generator fails to function correctly, in which
// case the caller should not continue.
func (r *Randomizer) Bytes(n int) ([]byte, error) {
	b := make([]byte, n)
	_, err := cryptoRand.Read(b)
	// Note that err == nil only if we read len(b) bytes.
	if err != nil {
		return nil, err
	}

	return b, nil
}

// String returns a URL-safe, base64 encoded
// securely generated random string.
func (r *Randomizer) String(s int) (string, error) {
	b, err := r.Bytes(s)
	return base64.URLEncoding.EncodeToString(b), err
}

// Number generates a number between 0 and n
func (r *Randomizer) Number(n int) int {
	return mathRand.Intn(n)
}

// NumberBetween returns a random number between n and m
func (r *Randomizer) NumberBetween(n int, m int) int {
	return mathRand.Intn(m-n) + n
}

// Port asks the kernel for an available port
func (r *Randomizer) Port() (int, error) {
	addr, err := net.ResolveTCPAddr("tcp", "localhost:0")
	if err != nil {
		return 0, err
	}

	l, err := net.ListenTCP("tcp", addr)
	if err != nil {
		return 0, err
	}
	defer l.Close()
	return l.Addr().(*net.TCPAddr).Port, nil
}

// Code returns a random code
func (r *Randomizer) Code() string {
	return fmt.Sprintf("%016d", rand.Int63n(1e16))
}

// New returns a preseeded randomizer
func New() *Randomizer {
	randomizer := mathRand.New(mathRand.NewSource(time.Now().UTC().UnixNano()))
	if randomizer == nil {
		panic("Failed to start random number generator")
	}
	return &Randomizer{
		*randomizer,
	}
}
