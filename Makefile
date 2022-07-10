build:
	@go build -o bin/railway -ldflags "-X main.OriginalInstallationMethod=curl"

build_npm:
	@go build -o bin/railway -ldflags "-X main.OriginalInstallationMethod=npm"

build_brew:
	@go build -o bin/railway -ldflags "-X main.OriginalInstallationMethod=brew"

run:
	@go run main.go
