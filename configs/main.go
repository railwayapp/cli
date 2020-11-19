package configs

import (
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

	return config.viper.WriteConfig()
}

func New() *Configs {
	// Configs stored in root (~/.railway)
	// Includes token, etc
	rootViper := viper.New()
	rootConfigPath := path.Join(os.Getenv("HOME"), ".railway/config.json")
	rootViper.SetConfigFile(rootConfigPath)
	rootViper.ReadInConfig()

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
	projectViper.ReadInConfig()

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
