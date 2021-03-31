package configs

import (
	"fmt"
	"os"
	"path"
	"path/filepath"
	"reflect"

	"github.com/spf13/viper"
)

type Config struct {
	viper      *viper.Viper
	configPath string
}

type Configs struct {
	rootConfigs            *Config
	projectConfigs         *Config
	RailwayProductionToken string
	RailwayEnvFilePath     string
}

func IsDevMode() bool {
	environment, exists := os.LookupEnv("RAILWAY_ENV")
	return exists && environment == "develop"
}

func (c *Configs) CreatePathIfNotExist(path string) error {
	dir := filepath.Dir(path)

	if _, err := os.Stat(dir); os.IsNotExist(err) {
		err = os.MkdirAll(dir, os.ModePerm)
		if err != nil {
			return err
		}
	}

	return nil
}

func (c *Configs) unmarshalConfig(config *Config, data interface{}) error {
	err := config.viper.ReadInConfig()
	if err != nil {
		return err
	}
	return config.viper.Unmarshal(&data)
}

func (c *Configs) marshalConfig(config *Config, cfg interface{}) error {
	reflectCfg := reflect.ValueOf(cfg)
	for i := 0; i < reflectCfg.NumField(); i++ {
		k := reflectCfg.Type().Field(i).Name
		v := reflectCfg.Field(i).Interface()

		config.viper.Set(k, v)
	}

	err := c.CreatePathIfNotExist(config.configPath)

	if err != nil {
		return err
	}

	return config.viper.WriteConfig()
}

func New() *Configs {
	// Configs stored in root (~/.railway)
	// Includes token, etc
	rootViper := viper.New()
	rootConfigPartialPath := ".railway/config.json"
	if IsDevMode() {
		rootConfigPartialPath = ".railway/dev-config.json"
	}
	rootConfigPath := path.Join(os.Getenv("HOME"), rootConfigPartialPath)
	rootViper.SetConfigFile(rootConfigPath)
	err := rootViper.ReadInConfig()
	if os.IsNotExist(err) {
		// That's okay, configs are created as needed
	} else if err != nil {
		fmt.Printf("Unable to parse railway config! %s\n", err)
	}

	rootConfig := &Config{
		viper:      rootViper,
		configPath: rootConfigPath,
	}

	// Configs stored in projects (<project>/.railway)
	// Includes projectId, environmentId, etc
	projectDir, err := filepath.Abs("./.railway")
	if err != nil {
		panic(err)
	}
	projectViper := viper.New()

	projectPath := path.Join(projectDir, "./config.json")
	projectViper.SetConfigFile(projectPath)
	err = projectViper.ReadInConfig()
	if os.IsNotExist(err) {
		// That's okay, configs are created as needed
	} else if err != nil {
		fmt.Printf("Unable to parse project config! %s\n", err)
	}

	projectConfig := &Config{
		viper:      projectViper,
		configPath: projectPath,
	}

	return &Configs{
		projectConfigs:         projectConfig,
		rootConfigs:            rootConfig,
		RailwayProductionToken: os.Getenv("RAILWAY_TOKEN"),
		RailwayEnvFilePath:     path.Join(projectDir, "env.json"),
	}
}
