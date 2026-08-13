FROM sysg-usecase-base
COPY tests/docker/model/stack.yaml /etc/systemg/systemg.yaml
COPY tests/docker/model/run_model_tests.sh /run.sh
RUN chmod +x /run.sh
ENTRYPOINT ["/run.sh"]
